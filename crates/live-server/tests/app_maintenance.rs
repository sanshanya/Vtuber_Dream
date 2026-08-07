//! §8.6 手动图维护端点钉团：POST /api/rooms/:uid/maintenance/entity_split|entity_merge。
//! 钉面：200 应用 / 幂等重放（changed=false + run 账不增生）/ 404（错房间、无图、
//! 未知实体/mention）/ 422（体形状、空数组、source==target、mention 归属外省）。
//!
//! 布景 = yaml_template + 真实落盘图文件（端点是图的写缝，必须走文件库）。

use serde_json::{Value, json};
use tower::ServiceExt;

mod common;

use axum::http::Request;
use http_body_util::BodyExt;

use live_core::episodes::{Episode, EpisodeField};
use live_core::graph::build::apply_viewer_submission;
use live_core::graph::store::Store;
use live_server::app::{AppState, build_app};

const RUN_A: &str = "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    data_root: std::path::PathBuf,
    entity: String,
    other_entity: String,
    m1: String,
    m2: String,
}

fn seed_graph(db_path: &std::path::Path) -> (String, String, String, String) {
    let store = Store::open(db_path).unwrap();
    store
        .begin_run_fixed(RUN_A, "2026-08-03T00:00:00+00:00", "m")
        .unwrap();
    let submission: live_core::models::ViewerPerceptionSubmission = serde_json::from_value(json!({
        "viewer_id": "v",
        "profile_summary": "该观众近期集中关注演示作品，优先新内容。",
        "mentions": [
            {"mention_id": "m1", "episode_id": "episode:v:ep1", "field_path": "title",
             "text": "环宝", "start": 0, "end": 2, "mention_type": "作品",
             "origin": "explicit", "proposed_entity_name": "异环",
             "proposed_entity_type": "game", "entity_ref": "entity:e1", "confidence": 0.8},
            {"mention_id": "m2", "episode_id": "episode:v:ep1", "field_path": "title",
             "text": "异环", "start": 3, "end": 5, "mention_type": "作品",
             "origin": "explicit", "proposed_entity_name": "异环",
             "proposed_entity_type": "game", "entity_ref": "entity:e1", "confidence": 0.8},
            {"mention_id": "m3", "episode_id": "episode:v:ep1", "field_path": "title",
             "text": "明日方舟", "start": 6, "end": 10, "mention_type": "作品",
             "origin": "explicit", "proposed_entity_name": "明日方舟",
             "proposed_entity_type": "game", "entity_ref": "entity:e2", "confidence": 0.7},
        ],
        "entities": [
            {"local_id": "e1", "canonical_name": "异环", "entity_type": "game",
             "aliases": ["环"], "description": "d", "existing_entity_id": null,
             "resolution": "NEW_ENTITY", "evidence_mention_ids": ["m1", "m2"],
             "parent_entity_refs": [], "confidence": 0.8},
            {"local_id": "e2", "canonical_name": "明日方舟", "entity_type": "game",
             "aliases": [], "description": "d", "existing_entity_id": null,
             "resolution": "NEW_ENTITY", "evidence_mention_ids": ["m3"],
             "parent_entity_refs": [], "confidence": 0.7},
        ],
        "relations": [],
        "interest_states": [
            {"entity_ref": "entity:e1", "status": "近期上升", "preference": "关注具体内容",
             "aspects": [], "rationale": "r", "evidence_mention_ids": ["m1"],
             "confidence": 0.5},
        ],
        "content_preferences": [], "recent_changes": [], "hypotheses": [],
        "conversation_openers": [], "content_ideas": [], "enrichment_targets": [],
        "cautions": [], "leads": []
    }))
    .unwrap();
    let episode = Episode {
        episode_id: "episode:v:ep1".to_string(),
        viewer_id: "v".to_string(),
        source: "bilibili".to_string(),
        event_type: "video".to_string(),
        observed_at: "2026-08-03T00:00:00+00:00".to_string(),
        published_at: "2026-08-03T00:00:00+00:00".to_string(),
        title: "样例".to_string(),
        url: "https://example.com/v".to_string(),
        bvid: "BV1xx4y1c7Ef".to_string(),
        fields: vec![EpisodeField {
            path: "title".to_string(),
            text: "环宝 异环 明日方舟".to_string(),
            kind: "title".to_string(),
        }],
        platform_facts: json!({}),
    };
    apply_viewer_submission(&store, RUN_A, "观众V", &[episode], &submission).unwrap();
    let pick = |sql: &str, p: &str| -> String {
        store.conn.query_row(sql, [p], |row| row.get(0)).unwrap()
    };
    let entity = pick(
        "SELECT entity_id FROM entities WHERE canonical_name=?",
        "异环",
    );
    let other_entity = pick(
        "SELECT entity_id FROM entities WHERE canonical_name=?",
        "明日方舟",
    );
    let m1 = pick("SELECT mention_id FROM mentions WHERE text=?", "环宝");
    let m2 = pick("SELECT mention_id FROM mentions WHERE text=?", "异环");
    // 显式断开连接：写库生命周期在测试内闭合成句，端点另开新连接。
    drop(store);
    (entity, other_entity, m1, m2)
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    std::fs::create_dir_all(output_dir.join("graph")).unwrap();
    let (entity, other_entity, m1, m2) =
        seed_graph(&output_dir.join("graph").join("perception.sqlite3"));
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "g1-maintenance",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "g1-maintenance",
        )
        .replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    let app = build_app(AppState {
        config_path,
        web_root: tmp.path().join("no-dist"),
        registry: live_server::registry::Registry::new(),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });
    Fixture {
        _tmp: tmp,
        app,
        data_root: output_dir,
        entity,
        other_entity,
        m1,
        m2,
    }
}

async fn oneshot(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body
        .map(|value| axum::body::Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap_or_else(axum::body::Body::empty);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn reopen(fx: &Fixture) -> Store {
    Store::open(&fx.data_root.join("graph").join("perception.sqlite3")).unwrap()
}

fn graph_count(store: &Store, sql: &str) -> i64 {
    store.conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

// ---------------------------------------------------------------------------
// split：200 应用 + 幂等重放
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn entity_split_applies_then_replays_idempotently() {
    let fx = fixture();
    let body = json!({"entity_id": fx.entity, "mention_ids": [fx.m1]});
    let (status, applied) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_split",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["op"], "entity_split");
    assert_eq!(applied["changed"], true, "{applied}");
    assert_eq!(applied["entity_id"], fx.entity.as_str());
    assert_eq!(applied["mention_ids"], json!([fx.m1]));
    assert!(
        applied["new_entity_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(applied["run_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(applied["moved_mentions"], 1);
    assert_eq!(applied["closed_edges"], 1, "布景只有兴趣边证据落 m1");

    let store = reopen(&fx);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM graph_runs"), 2);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM entities"), 3);
    drop(store);

    // 重放：同参 = 同终态，run 账不增生，run_id 指回原始的维护 run。
    let (status, replayed) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_split",
        Some(body),
    )
    .await;
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["changed"], false, "{replayed}");
    assert_eq!(replayed["new_entity_id"], applied["new_entity_id"]);
    assert_eq!(replayed["run_id"], applied["run_id"]);
    let store = reopen(&fx);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM graph_runs"), 2);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM entities"), 3);
    assert_eq!(
        graph_count(
            &store,
            "SELECT COUNT(*) FROM graph_runs WHERE kind='maintenance'"
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// split：404 面（错房间 / 无图房间 / 未知实体 / 未知 mention）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn entity_split_not_found_surface() {
    let fx = fixture();
    let body = json!({"entity_id": fx.entity, "mention_ids": [fx.m1]});
    // 错房间（单房间布局）
    let (status, resp) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/999/maintenance/entity_split",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, 404, "{resp}");
    // 未知实体
    let (status, resp) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_split",
        Some(json!({"entity_id": "entity:game:ghost", "mention_ids": [fx.m1]})),
    )
    .await;
    assert_eq!(status, 404, "{resp}");
    assert!(resp["error"].as_str().unwrap().contains("不存在"), "{resp}");
    // 未知 mention
    let (status, resp) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_split",
        Some(json!({"entity_id": fx.entity, "mention_ids": ["mention:v:ghost"]})),
    )
    .await;
    assert_eq!(status, 404, "{resp}");
    // 无图房间：另起无 graph 文件的输出根
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "g1-maintenance-bare",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "g1-maintenance-bare",
        )
        .replace("OUTPUT_DIR", &bare.display().to_string().replace('\\', "/")),
    )
    .unwrap();
    let bare_app = build_app(AppState {
        config_path,
        web_root: tmp.path().join("no-dist"),
        registry: live_server::registry::Registry::new(),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });
    let (status, resp) = oneshot(
        &bare_app,
        "POST",
        "/api/rooms/983/maintenance/entity_split",
        Some(body),
    )
    .await;
    assert_eq!(status, 404, "{resp}");
}

// ---------------------------------------------------------------------------
// split：422 面（体形状 / 空数组 / 非字符串成员 / mention 归属外省）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn entity_split_unprocessable_surface() {
    let fx = fixture();
    for bad in [
        json!({"mention_ids": [fx.m1]}),                     // 缺 entity_id
        json!({"entity_id": "", "mention_ids": [fx.m1]}),    // 空 entity_id
        json!({"entity_id": fx.entity, "mention_ids": []}),  // 空数组
        json!({"entity_id": fx.entity, "mention_ids": [1]}), // 非字符串成员
        json!({"entity_id": fx.entity}),                     // 缺 mention_ids
        json!(["not", "an", "object"]),                      // 非对象体
    ] {
        let (status, resp) = oneshot(
            &fx.app,
            "POST",
            "/api/rooms/983/maintenance/entity_split",
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, 422, "{bad} → {resp}");
        assert!(resp["error"].as_str().is_some(), "{bad} → {resp}");
    }
    // mention 归属外省：m3(明日方舟) 不属 e1 → 422 且错文点名归属。
    let (status, resp) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_split",
        Some(json!({"entity_id": fx.entity, "mention_ids": [fx.m2, "mention:v:1"]})),
    )
    .await;
    assert_eq!(status, 404, "{resp}"); // mention:v:1 不存在 → 404 优先
    let foreign: String = {
        let store = reopen(&fx);
        store
            .conn
            .query_row(
                "SELECT mention_id FROM mentions WHERE text=?",
                ["明日方舟"],
                |row| row.get(0),
            )
            .unwrap()
    };
    let (status, resp) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_split",
        Some(json!({"entity_id": fx.entity, "mention_ids": [foreign]})),
    )
    .await;
    assert_eq!(status, 422, "{resp}");
    assert!(resp["error"].as_str().unwrap().contains("不属于"), "{resp}");
    // 报错面零写入：run 账与实体数冻结。
    let store = reopen(&fx);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM graph_runs"), 1);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM entities"), 2);
}

// ---------------------------------------------------------------------------
// merge：200 应用 + 幂等重放 + 404/422 面
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn entity_merge_applies_then_replays_idempotently() {
    let fx = fixture();
    let body = json!({"source_ids": [fx.entity], "target_id": fx.other_entity});
    let (status, applied) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_merge",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["op"], "entity_merge");
    assert_eq!(applied["changed"], true, "{applied}");
    assert_eq!(applied["source_ids"], json!([fx.entity]));
    assert_eq!(applied["target_id"], fx.other_entity.as_str());
    assert!(applied["run_id"].as_str().is_some_and(|s| !s.is_empty()));
    let store = reopen(&fx);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM entities"), 1);
    assert_eq!(
        graph_count(
            &store,
            "SELECT COUNT(*) FROM graph_runs WHERE kind='maintenance'"
        ),
        1
    );
    // 别名合流可视：目标实体持 环/异环/明日方舟 三 key（环 与 异环 来自源）。
    let alias_keys: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM entity_aliases WHERE entity_id=?",
            [fx.other_entity.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(alias_keys, 3);
    drop(store);

    let (status, replayed) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_merge",
        Some(body),
    )
    .await;
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["changed"], false, "{replayed}");
    assert_eq!(replayed["run_id"], applied["run_id"]);
    let store = reopen(&fx);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM graph_runs"), 2);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM entities"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_merge_error_surface() {
    let fx = fixture();
    // 404：未知源 / 未知目标
    let (status, resp) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_merge",
        Some(json!({"source_ids": ["entity:game:ghost"], "target_id": fx.other_entity})),
    )
    .await;
    assert_eq!(status, 404, "{resp}");
    let (status, resp) = oneshot(
        &fx.app,
        "POST",
        "/api/rooms/983/maintenance/entity_merge",
        Some(json!({"source_ids": [fx.entity], "target_id": "entity:game:ghost"})),
    )
    .await;
    assert_eq!(status, 404, "{resp}");
    // 422：source==target / 空数组 / 缺键
    for bad in [
        json!({"source_ids": [fx.entity], "target_id": fx.entity}),
        json!({"source_ids": [], "target_id": fx.other_entity}),
        json!({"target_id": fx.other_entity}),
        json!({"source_ids": [fx.entity]}),
    ] {
        let (status, resp) = oneshot(
            &fx.app,
            "POST",
            "/api/rooms/983/maintenance/entity_merge",
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, 422, "{bad} → {resp}");
    }
    let store = reopen(&fx);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM entities"), 2);
    assert_eq!(graph_count(&store, "SELECT COUNT(*) FROM graph_runs"), 1);
}
