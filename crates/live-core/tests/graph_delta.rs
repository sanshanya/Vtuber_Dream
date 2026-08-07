//! M5 G4：run_pair_delta 钉（相邻 complete as-of 差分 + guards 窗口 + 基线态）。
//! 布景 = 手工 SQL 塞表（同法）；时间戳全用 ISO 字面，as-of 切窗可复读。

use serde_json::{Value, json};

use live_core::graph::query::run_pair_delta;
use live_core::graph::store::Store;

const T0: &str = "2026-07-31T00:00:00"; // 边基线起点
const T1: &str = "2026-08-01T00:00:00"; // run-1 complete
const T_CLOSE: &str = "2026-08-01T06:00:00"; // v1→e3 关闭点
const T_CHANGE: &str = "2026-08-01T12:00:00"; // v1→e2 签名变更点
const T_OPEN: &str = "2026-08-01T13:00:00"; // v2→e9 起点
const T_GUARD: &str = "2026-08-01T14:00:00"; // v1 上舰点
const T_GUARD_OFF: &str = "2026-08-01T20:00:00"; // v0 下舰点
const T2: &str = "2026-08-02T00:00:00"; // run-2 complete

fn insert_run(store: &Store, run_id: &str, completed_at: Option<&str>) {
    match completed_at {
        Some(at) => store
            .conn
            .execute(
                "INSERT INTO graph_runs(run_id, started_at, completed_at) VALUES(?,?,?)",
                rusqlite::params![run_id, T0, at],
            )
            .unwrap(),
        // failed run：completed_at = NULL——必须不出现在「相邻 complete」候选里。
        None => store
            .conn
            .execute(
                "INSERT INTO graph_runs(run_id, started_at, failed_at) VALUES(?,?,?)",
                rusqlite::params![run_id, T0, T1],
            )
            .unwrap(),
    };
}

fn insert_viewer(store: &Store, id: &str) {
    store
        .conn
        .execute(
            "INSERT INTO nodes(node_id,node_type,name,source_kind,first_seen_at,last_seen_at) \
             VALUES(?, 'Viewer', ?, 'platform_fact', ?, ?)",
            rusqlite::params![format!("viewer:{id}"), id, T0, T0],
        )
        .unwrap();
}

fn insert_entity(store: &Store, id: &str, name: &str) {
    // edges 两端外键指 nodes ── 真实构建双写 entities + nodes（同 node_id）。
    store
        .conn
        .execute(
            "INSERT INTO nodes(node_id,node_type,name,source_kind,first_seen_at,last_seen_at) \
             VALUES(?, 'Entity', ?, 'grounded_ai', ?, ?)",
            rusqlite::params![format!("entity:{id}"), name, T0, T0],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,source_kind,\
             first_seen_at,last_seen_at) VALUES(?,?,?,?,?,?,?)",
            rusqlite::params![
                format!("entity:{id}"),
                name,
                name,
                "Topic",
                "grounded_ai",
                T0,
                T0
            ],
        )
        .unwrap();
}

fn insert_edge(
    store: &Store,
    edge_id: &str,
    source: &str,
    predicate: &str,
    target: &str,
    props: Value,
    window: (&str, Option<&str>),
) {
    store
        .conn
        .execute(
            "INSERT INTO edges(edge_id,source_id,predicate,target_id,properties_json,source_kind,\
             confidence,valid_from,valid_to,first_seen_at,last_seen_at,run_id) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                edge_id,
                source,
                predicate,
                target,
                serde_json::to_string(&props).unwrap(),
                if predicate == "INTERESTED_IN" {
                    "ai_state"
                } else {
                    "platform_fact"
                },
                0.9,
                window.0,
                window.1,
                window.0,
                window.1.unwrap_or(window.0),
                "run-1",
            ],
        )
        .unwrap();
}

fn staging() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("perception.sqlite3")).expect("store opens");
    for viewer in ["v1", "v2", "v0", "s1"] {
        insert_viewer(&store, viewer);
    }
    insert_entity(&store, "e1", "异环");
    insert_entity(&store, "e2", "明日方舟");
    insert_entity(&store, "e3", "旧爱");
    insert_entity(&store, "e9", "新欢");
    // 边集合挂 run-1（FK）；run-2/failed 行由用例各自的剧本追加。
    insert_run(&store, "run-1", Some(T1));

    // 1) 全程未动 → 任何清单未见
    insert_edge(
        &store,
        "keep",
        "viewer:v1",
        "INTERESTED_IN",
        "entity:e1",
        json!({"status": "active", "preference": "keep"}),
        (T0, None),
    );
    // 2) 签名变更：窗口内旧区间关闭 + 新区间同对开启 → changed
    insert_edge(
        &store,
        "change-old",
        "viewer:v1",
        "INTERESTED_IN",
        "entity:e2",
        json!({"status": "active", "preference": "x"}),
        (T0, Some(T_CHANGE)),
    );
    insert_edge(
        &store,
        "change-new",
        "viewer:v1",
        "INTERESTED_IN",
        "entity:e2",
        json!({"status": "fading", "preference": "y"}),
        (T_CHANGE, None),
    );
    // 3) 窗口内关闭 → closed
    insert_edge(
        &store,
        "drop",
        "viewer:v1",
        "INTERESTED_IN",
        "entity:e3",
        json!({"status": "sleeping", "preference": "c"}),
        (T0, Some(T_CLOSE)),
    );
    // 4) 窗口内开启 → opened
    insert_edge(
        &store,
        "open",
        "viewer:v2",
        "INTERESTED_IN",
        "entity:e9",
        json!({"status": "watching", "preference": "o"}),
        (T_OPEN, None),
    );
    // guards：窗口内上舰 + 下舰各一；条法与谓词纯度：平台事实边谓词混入不应干扰
    insert_edge(
        &store,
        "guard-add",
        "viewer:v1",
        "GUARD_OF",
        "viewer:s1",
        json!({}),
        (T_GUARD, None),
    );
    insert_edge(
        &store,
        "guard-off",
        "viewer:v0",
        "GUARD_OF",
        "viewer:s1",
        json!({}),
        (T0, Some(T_GUARD_OFF)),
    );
    (tmp, store)
}

#[test]
fn run_pair_delta_interest_and_guards_window() {
    let (_tmp, store) = staging();
    // staging 已含 run-1；补：run-0 更旧 + run-2 并重 + failed run（不计入相邻 complete）。
    insert_run(&store, "run-0", Some("2026-07-01T00:00:00"));
    insert_run(&store, "run-2", Some(T2));
    insert_run(&store, "run-3-failed", None);

    let delta = run_pair_delta(&store).expect("delta builds");
    assert_eq!(delta["baseline_only"], false);
    assert_eq!(delta["from_run_id"], "run-1");
    assert_eq!(delta["to_run_id"], "run-2");

    let interest = &delta["interest"];
    assert_eq!(
        interest["opened"],
        json!([{
            "viewer_id": "v2",
            "entity_id": "entity:e9",
            "canonical_name": "新欢",
            "status": "watching",
            "preference": "o",
        }])
    );
    assert_eq!(
        interest["closed"],
        json!([{
            "viewer_id": "v1",
            "entity_id": "entity:e3",
            "canonical_name": "旧爱",
            "status": "sleeping",
            "preference": "c",
        }])
    );
    assert_eq!(
        interest["changed"],
        json!([{
            "viewer_id": "v1",
            "entity_id": "entity:e2",
            "canonical_name": "明日方舟",
            "from": {"status": "active", "preference": "x"},
            "to": {"status": "fading", "preference": "y"},
        }])
    );
    let serialized = serde_json::to_string(&delta).unwrap();
    assert!(
        !serialized.contains("异环"),
        "未动的 e1 不得入差分：{serialized}"
    );

    assert_eq!(delta["guards"]["added"], json!(["v1"]));
    assert_eq!(delta["guards"]["removed"], json!(["v0"]));
}

#[test]
fn run_pair_delta_baseline_when_fewer_than_two_complete_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("g.sqlite3")).expect("store opens");
    // 空库 → 基线态
    let delta = run_pair_delta(&store).expect("delta builds");
    assert_eq!(delta["baseline_only"], true);
    assert_eq!(delta["from_run_id"], Value::Null);
    assert_eq!(delta["interest"]["opened"], json!([]));
    assert_eq!(delta["guards"]["added"], json!([]));

    // 单次 complete run → 仍基线态（面板显示「基线已建」）
    insert_run(&store, "run-1", Some(T1));
    let delta = run_pair_delta(&store).expect("delta builds");
    assert_eq!(delta["baseline_only"], true);
    assert_eq!(delta["to_run_id"], Value::Null);
}

/// 复盘解耦：recap-refresh 类 run（collect 尾的四个数刷新）虽照常
/// complete，但绝不进「相邻 complete」对照窗——夹心两条 refresh 后配对必须
/// 仍是 (run-1, run-2)，「vs 上轮感知」不被「每日只收」稀释成无变化。
#[test]
fn recap_refresh_runs_stay_out_of_pairing() {
    let (_tmp, store) = staging();
    insert_run(&store, "run-2", Some(T2));
    for (run_id, at) in [
        ("run-refresh-1", "2026-08-01T18:00:00"),
        ("run-refresh-2", "2026-08-01T23:00:00"),
    ] {
        store
            .conn
            .execute(
                "INSERT INTO graph_runs(run_id, started_at, completed_at, kind) VALUES(?,?,?,?)",
                rusqlite::params![run_id, T0, at, Store::RUN_KIND_RECAP_REFRESH],
            )
            .unwrap();
    }
    let delta = run_pair_delta(&store).expect("delta builds");
    assert_eq!(delta["baseline_only"], false);
    assert_eq!(delta["from_run_id"], "run-1");
    assert_eq!(delta["to_run_id"], "run-2");
}
