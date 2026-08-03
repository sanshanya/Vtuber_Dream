//! 黄金样本对账（设计文档 M1 验收）：
//! tests-fixtures/demo/viewers/*.json → episodes → apply 三份观众提交 + 整体态势提交
//! → 七表与 tests-fixtures/demo/graph_dump.json 比对。
//!
//! 对比口径（dump 由 Python v5 生成）：
//! - 丢弃所有时间戳列（first_seen_at / last_seen_at / created_at / valid_from /
//!   started_at / completed_at）：注入式时钟下不可复现，也无语义；
//! - 丢弃 edges.edge_id（含时间成分，不可复现）；
//! - v6 显式升级清单：edges.viewer_id 列不导出（纯索引列）；ai_action 边
//!   （TARGETS/ABOUT）confidence 在 v5 dump 中为 NULL、v6 非空——忽略该列。
//!
//! 另含：INTERESTED_IN 幂等回归（§8.2）与内容变化区间切换断言。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use live_core::episodes::{build_viewer_episodes, json_canon};
use live_core::graph::build::{apply_audience_submission, apply_viewer_submission};
use live_core::graph::store::Store;
use live_core::models::{AudienceSituationSubmission, ViewerPerceptionSubmission};
use serde_json::{Map, Value};

const FIXED_NOW: &str = "2026-08-03T07:02:19.191879+00:00";
const FIXTURE_RUN_ID: &str = "run:fec330f10bc04814beda1adf43d91668";
const FIXTURE_MODEL: &str = "custom-reasoning-model";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-fixtures/demo")
}

fn load_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn viewer_submission(name: &str) -> ViewerPerceptionSubmission {
    let raw = load_json(&fixtures().join(format!("ai/perception/viewers/{name}.json")));
    serde_json::from_value(raw["analysis"].clone()).unwrap()
}

fn build_demo_graph() -> Store {
    let store =
        Store::open_with_clock(Path::new(":memory:"), Box::new(|| FIXED_NOW.to_string())).unwrap();
    store
        .begin_run_fixed(
            FIXTURE_RUN_ID,
            "2026-08-03T07:02:19.188881+00:00",
            FIXTURE_MODEL,
        )
        .unwrap();
    for name in ["demo-1", "demo-2", "demo-3"] {
        let viewer = load_json(&fixtures().join(format!("viewers/{name}.json")));
        let episodes = build_viewer_episodes(&viewer, 1000);
        let viewer_name = viewer["viewer"]["name"].as_str().unwrap().to_string();
        let submission = viewer_submission(name);
        apply_viewer_submission(&store, FIXTURE_RUN_ID, &viewer_name, &episodes, &submission)
            .unwrap();
    }
    let situation = load_json(&fixtures().join("ai/situation.json"));
    let submission: AudienceSituationSubmission =
        serde_json::from_value(situation["analysis"].clone()).unwrap();
    apply_audience_submission(&store, FIXTURE_RUN_ID, &submission).unwrap();
    store
}

const DROP_COLUMNS: [&str; 7] = [
    "first_seen_at",
    "last_seen_at",
    "created_at",
    "valid_from",
    "started_at",
    "completed_at",
    "failed_at",
];

fn export_table(store: &Store, table: &str) -> Vec<String> {
    let sql = format!("SELECT * FROM {table}");
    let mut stmt = store.conn.prepare(&sql).unwrap();
    let columns: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let rows = stmt
        .query_map([], |row| {
            let mut map = Map::new();
            for (index, column) in columns.iter().enumerate() {
                if DROP_COLUMNS.contains(&column.as_str()) {
                    continue;
                }
                // edges.viewer_id 是 v6 纯索引列（语义=properties/source 推导），不导出；
                // episodes.viewer_id 是 v5 既有列，必须保留比对。
                if table == "edges" && column == "viewer_id" {
                    continue;
                }
                let value: rusqlite::types::Value = row.get(index)?;
                let json = match value {
                    rusqlite::types::Value::Null => Value::Null,
                    rusqlite::types::Value::Integer(n) => Value::from(n),
                    rusqlite::types::Value::Real(f) => serde_json::Number::from_f64(f)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                    rusqlite::types::Value::Text(s) => Value::String(s),
                    rusqlite::types::Value::Blob(b) => {
                        Value::String(String::from_utf8_lossy(&b).into_owned())
                    }
                };
                map.insert(column.clone(), json);
            }
            Ok(map)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut rows: Vec<String> = rows
        .into_iter()
        .map(|row| json_canon(&Value::Object(row)))
        .collect();
    rows.sort();
    rows
}

fn expected_table(dump: &Value, table: &str) -> Vec<String> {
    let mut rows: Vec<String> = dump[table]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let mut map = row.as_object().unwrap().clone();
            for column in DROP_COLUMNS {
                map.remove(column);
            }
            if table == "edges" {
                map.remove("edge_id");
                // v6 升级：TARGETS/ABOUT 必带 confidence，v5 dump 为 NULL。
                if map.get("source_kind").and_then(Value::as_str) == Some("ai_action") {
                    map.remove("confidence");
                }
            }
            if table == "mentions" {
                map.remove("created_at");
            }
            json_canon(&Value::Object(map))
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn episode_ids_match_fixture() {
    let viewer = load_json(&fixtures().join("viewers/demo-1.json"));
    let episodes = build_viewer_episodes(&viewer, 1000);
    assert_eq!(episodes.len(), 1);
    assert_eq!(
        episodes[0].episode_id,
        "episode:demo-1:39581170fe829af2:e431120c067efe82"
    );
    let viewer3 = load_json(&fixtures().join("viewers/demo-3.json"));
    let episodes3 = build_viewer_episodes(&viewer3, 1000);
    assert_eq!(
        episodes3[0].episode_id,
        "episode:demo-3:49dfc5a3f38413ba:63d4193773a2d43b"
    );
}

#[test]
fn golden_graph_dump_reconciliation() {
    let store = build_demo_graph();
    let dump = load_json(&fixtures().join("graph_dump.json"));
    for table in [
        "graph_runs",
        "episodes",
        "mentions",
        "entities",
        "entity_aliases",
        "nodes",
        "edges",
    ] {
        let mut actual = export_table(&store, table);
        let mut expected = expected_table(&dump, table);
        if table == "edges" {
            actual = actual
                .into_iter()
                .map(|row| {
                    let mut map: Map<String, Value> = serde_json::from_str(&row).unwrap();
                    map.remove("edge_id");
                    if map.get("source_kind").and_then(Value::as_str) == Some("ai_action") {
                        map.remove("confidence");
                    }
                    json_canon(&Value::Object(map))
                })
                .collect();
            actual.sort();
            expected.sort();
        }
        if table == "graph_runs" {
            // 仅 run_id + model 可复现（时间戳与失败列已丢弃、failure_json 为空）。
            let key = |rows: &[String]| -> Vec<String> {
                rows.iter()
                    .map(|row| {
                        let map: Map<String, Value> = serde_json::from_str(row).unwrap();
                        let mut slim = Map::new();
                        slim.insert("run_id".to_string(), map["run_id"].clone());
                        slim.insert("model".to_string(), map["model"].clone());
                        json_canon(&Value::Object(slim))
                    })
                    .collect()
            };
            assert_eq!(key(&actual), key(&expected), "graph_runs mismatch");
            continue;
        }
        assert_eq!(
            actual.len(),
            expected.len(),
            "{table} row count mismatch\nactual:\n{}\nexpected:\n{}",
            actual.join("\n"),
            expected.join("\n")
        );
        for (index, (got, want)) in actual.iter().zip(expected.iter()).enumerate() {
            assert_eq!(got, want, "{table} row {index} mismatch");
        }
    }
}

// ---------------------------------------------------------------------------
// §8.2 回归：INTERESTED_IN 幂等 + 变化切换区间
// ---------------------------------------------------------------------------

fn interest_edges(store: &Store) -> BTreeSet<String> {
    let mut stmt = store
        .conn
        .prepare(
            "SELECT edge_id FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL ORDER BY edge_id",
        )
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .unwrap()
}

fn apply_demo1(store: &Store, run_id: &str, tweak: impl Fn(&mut ViewerPerceptionSubmission)) {
    store
        .begin_run_fixed(run_id, FIXED_NOW, FIXTURE_MODEL)
        .unwrap();
    let viewer = load_json(&fixtures().join("viewers/demo-1.json"));
    let episodes = build_viewer_episodes(&viewer, 1000);
    let mut submission = viewer_submission("demo-1");
    tweak(&mut submission);
    apply_viewer_submission(store, run_id, "演示观众A", &episodes, &submission).unwrap();
}

#[test]
fn interest_state_rerun_is_idempotent() {
    let store =
        Store::open_with_clock(Path::new(":memory:"), Box::new(|| FIXED_NOW.to_string())).unwrap();
    apply_demo1(&store, "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", |_| {});
    let first = interest_edges(&store);
    assert_eq!(first.len(), 2);
    // 同内容换个 run 重跑：活跃边集合与 ID 不变（§8.2 保留 interval）。
    apply_demo1(&store, "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", |_| {});
    let second = interest_edges(&store);
    assert_eq!(first, second, "same content rerun must keep edge ids");
}

#[test]
fn interest_state_change_closes_interval_and_opens_new() {
    let store =
        Store::open_with_clock(Path::new(":memory:"), Box::new(|| FIXED_NOW.to_string())).unwrap();
    apply_demo1(&store, "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", |_| {});
    let before = interest_edges(&store);
    // 第二次运行：e2「角色演出」状态从 稳定 → 近期上升（内容变化）。
    apply_demo1(
        &store,
        "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        |submission| {
            submission.interest_states[1].status = "近期上升".to_string();
        },
    );
    let after = interest_edges(&store);
    // 活跃集合：e1 保留原边；e2 关闭旧边开新边 → 总数仍 2，但集合 diff。
    assert_eq!(after.len(), 2);
    let common: Vec<_> = before.intersection(&after).collect();
    assert_eq!(common.len(), 1, "unchanged state must keep its edge id");
    let closed: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        closed, 1,
        "changed state must close exactly one old interval"
    );
}
