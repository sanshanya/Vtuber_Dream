//! M5-D2 delta 两轮布景钉（D4 口径的 HTTP 面）+ 面板走查导出（env-gated）。
//!
//! 布景法：build_demo → 手工 SQL 在 demo 图内栽「第二个完整 run + 一条新生
//! INTERESTED_IN + 一条新 GUARD_OF」；as-of 窗口切窗与 graph_delta.rs 同法
//! （时间戳全用未来 ISO 字面，demo 自身的真实时钟 complete 行不再竞争相邻位）。
//!
//! GET /api/rooms/{uid}/overview 的 delta 区块必须非 baseline，且条目与布景一致。

use std::path::{Path, PathBuf};

use axum::http::Request;
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use live_server::app::{AppState, build_app};

/// 时间戳从测试运行时时钟派生（ag6 日历炸弹修复：硬编码 2027-01 曾使任何
/// 更晚的构建日让 demo 自己的 complete 行顶替「相邻」位而必红）。
/// 形状与 live_core::episodes::now_iso 同齿（秒 + +00:00）。
struct Clock {
    t1_base: String,
    t_edge: String,
    t2: String,
}

fn fixture_clock() -> Clock {
    let now = Utc::now();
    let stamp = |at: chrono::DateTime<Utc>| at.format("%Y-%m-%dT%H:%M:%S+00:00").to_string();
    Clock {
        t1_base: stamp(now + Duration::hours(8)),
        t_edge: stamp(now + Duration::hours(25)),
        t2: stamp(now + Duration::hours(49)),
    }
}

const YAML_TEMPLATE: &str = r#"
version: 6
project:
  name: m5d-delta
  output_dir: OUTPUT_DIR
bilibili:
  room_id: "983"
  streamer_uid: "9001"
  cookie: "SESSDATA=test"
  additional_viewer_ids: []
collection:
  max_guards: 50
  per_viewer_request_budget: 12
  followings_limit: 50
  recent_videos: 10
  recent_dynamics: 30
  favorite_folders: 3
  favorite_items_per_folder: 30
  bangumi_limit: 30
  games_limit: 30
  max_video_metadata_items: 120
  request_delay_seconds: 0
  timeout_seconds: 5
perception:
  peer_discovery:
    candidate_limit: 20
    recent_videos: 8
    recent_dynamics: 8
    max_formal_peers: 8
ai:
  api: chat_completions
  base_url: http://127.0.0.1:9/v1
  api_key: test-key
  model: m5d-delta
  reasoning:
    enabled: false
  agent:
    max_turns: 4
    run_retries: 0
  max_output_tokens: 131072
report:
  title: t
"#;

async fn get(app: &axum::Router, path: &str) -> (u16, Value) {
    let request = Request::builder()
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// 布景：demo + 第二 run + 双新边。返回 (tmp, config_path, demo_root)。
fn stage_delta_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    std::fs::create_dir_all(&output_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        YAML_TEMPLATE.replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    let config = live_core::config::load_config(&config_path).expect("config loads");
    let built = live_core::demo::build_demo(&config, None).expect("demo builds");
    let demo_root = PathBuf::from(built["output_dir"].as_str().expect("output_id"));

    let graph_db = demo_root.join("graph").join("perception.sqlite3");
    let conn = rusqlite::Connection::open(&graph_db).expect("graph opens");
    let clock = fixture_clock();
    // 异环实体（demo 自己的 NEW_ENTITY mint；rfind 语义 = node 行后写入者胜）
    let entity_id: String = conn
        .query_row(
            "SELECT node_id FROM nodes WHERE node_type='Entity' AND name='异环' ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("demo 图含 异环 实体");
    // 两个未来 complete run（相邻由 completed_at 排序确立）
    for (run_id, at) in [
        ("run-delta-a", clock.t1_base.as_str()),
        ("run-delta-b", clock.t2.as_str()),
    ] {
        conn.execute(
            "INSERT INTO graph_runs(run_id, started_at, completed_at) VALUES(?,?,?)",
            [run_id, clock.t1_base.as_str(), at],
        )
        .unwrap();
    }
    // edges 两端 FK → nodes：room:983 若 demo 未栽则补行（必须先于边 insert）。
    conn.execute(
        "INSERT OR IGNORE INTO nodes(node_id,node_type,name,source_kind,first_seen_at,last_seen_at) \
         VALUES('room:983', 'Room', '983', 'platform_fact', ?1, ?1)",
        [clock.t1_base.as_str()],
    )
    .unwrap();
    let edge = |edge_id: &str,
                source: &str,
                predicate: &str,
                target: &str,
                props: Value,
                kind: &str,
                conn: &rusqlite::Connection| {
        conn.execute(
            "INSERT INTO edges(edge_id,source_id,predicate,target_id,properties_json,source_kind,\
             confidence,valid_from,valid_to,first_seen_at,last_seen_at,run_id) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                edge_id,
                source,
                predicate,
                target,
                serde_json::to_string(&props).unwrap(),
                kind,
                0.9,
                clock.t_edge.as_str(),
                Option::<&str>::None,
                clock.t_edge.as_str(),
                clock.t_edge.as_str(),
                "run-delta-b",
            ],
        )
        .unwrap();
    };
    // ① demo-1 早已开 INTERESTED_IN 异环（demo 栽培）→ 变体属性 → changed
    edge(
        "edge-delta-interest",
        "viewer:demo-1",
        "INTERESTED_IN",
        &entity_id,
        json!({"status": "活跃", "preference": "关注具体内容"}),
        "ai_state",
        &conn,
    );
    // ② demo-1 上舰 → guards.added
    edge(
        "edge-delta-guard",
        "viewer:demo-1",
        "GUARD_OF",
        "room:983",
        json!({}),
        "platform_fact",
        &conn,
    );
    // ③ demo-2 → 新实体「原神」：as-of T1 未开，T2 打开 → interest.opened
    conn.execute(
        "INSERT INTO nodes(node_id,node_type,name,source_kind,first_seen_at,last_seen_at) \
         VALUES('entity:gs-os', 'Entity', '原神', 'grounded_ai', ?1, ?1)",
        [clock.t_edge.as_str()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,source_kind,\
         first_seen_at,last_seen_at) \
         VALUES('entity:gs-os','原神','原神','游戏','grounded_ai',?1,?1)",
        [clock.t_edge.as_str()],
    )
    .unwrap();
    edge(
        "edge-delta-opened",
        "viewer:demo-2",
        "INTERESTED_IN",
        "entity:gs-os",
        json!({"status": "活跃", "preference": "关注具体内容"}),
        "ai_state",
        &conn,
    );
    drop(conn);
    (tmp, config_path, demo_root)
}

#[tokio::test(flavor = "multi_thread")]
async fn overview_delta_after_second_complete_run_is_non_baseline() {
    let (_tmp, config_path, demo_root) = stage_delta_fixture();
    let app = build_app(AppState {
        config_path: config_path.clone(),
        web_root: Path::new("no-dist").to_path_buf(),
        registry: live_server::registry::Registry::new(),
        demo: true,
        data_root: Some(demo_root.clone()),
        bilibili_hosts: None,
    });
    let (status, overview) = get(&app, "/api/rooms/983/overview").await;
    assert_eq!(status, 200, "{overview}");
    let delta = &overview["delta"];
    assert_eq!(
        delta["baseline_only"], false,
        "第二个 complete run 布景后 delta 必须非基线：{delta}"
    );
    assert_eq!(delta["from_run_id"], "run-delta-a", "{delta}");
    assert_eq!(delta["to_run_id"], "run-delta-b", "{delta}");
    // 新开：demo-2 → 原神
    let opened = delta["interest"]["opened"].as_array().unwrap();
    assert_eq!(opened.len(), 1, "{delta}");
    assert_eq!(opened[0]["canonical_name"], "原神", "{opened:?}");
    assert_eq!(opened[0]["viewer_id"], "demo-2", "{opened:?}");
    assert_eq!(opened[0]["status"], "活跃", "{opened:?}");
    // 签名迁移：demo-1 异环 props 从 → 到（demo 原值 = build_demo 字面「关注具体内容与讨论角度」）。
    let changed = delta["interest"]["changed"].as_array().unwrap();
    assert_eq!(changed.len(), 1, "{delta}");
    assert_eq!(changed[0]["canonical_name"], "异环", "{changed:?}");
    assert_eq!(changed[0]["from"]["status"], "近期上升", "{changed:?}");
    assert_eq!(changed[0]["to"]["status"], "活跃", "{changed:?}");
    assert_eq!(
        changed[0]["from"]["preference"], "关注具体内容与讨论角度",
        "{changed:?}"
    );
    assert_eq!(
        changed[0]["to"]["preference"], "关注具体内容",
        "{changed:?}"
    );
    assert_eq!(delta["guards"]["added"], json!(["demo-1"]), "{delta}");
}

/// 面板走查导出（M5-D 验收路线：脚本化双运行 + diff 截图落私仓）。
///
/// env `VTD_WALK_SHOT` 打开：布景 → 起真端口 serve（web_root=web/dist）+ 打印
/// `WALK_SHOT_PORT` 与 demo 数据根——外部 playwright 工序据此截图。
#[test]
fn walk_delta_fixture_export() {
    if std::env::var("VTD_WALK_SHOT").is_err() {
        return;
    }
    let dump_dir = std::env::var("VTD_WALK_SHOT_DIR").unwrap_or_default();
    assert!(!dump_dir.is_empty(), "VTD_WALK_SHOT_DIR 必须指向导出目录");
    let dump_path = PathBuf::from(&dump_dir);
    std::fs::create_dir_all(&dump_path).unwrap();
    let (_tmp, config_path, demo_root) = stage_delta_fixture();
    let dst = dump_path.join("demo_root");
    if dst.exists() {
        std::fs::remove_dir_all(&dst).unwrap();
    }
    copy_tree(&demo_root, &dst);
    let cfg_dst = dump_path.join("config.yaml");
    std::fs::copy(&config_path, &cfg_dst).unwrap();
    eprintln!("WALK_SHOT_DATA_ROOT={}", dst.display());
    eprintln!("WALK_SHOT_CONFIG={}", cfg_dst.display());
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}
