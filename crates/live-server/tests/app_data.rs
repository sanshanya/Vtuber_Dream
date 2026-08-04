//! M5-B2 数据面钉团：overview（含 delta/leads 区块）/viewers/tree/graph（cytoscape DTO）。
//!
//! 布景 = build_demo 到临时输出根 → web/dist 无参与（fallback 另案钉）。

use serde_json::Value;
use tower::ServiceExt;

mod common;

use axum::http::Request;
use http_body_util::BodyExt;

use live_server::app::{AppState, build_app};

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    data_root: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "m5b-data",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "m5b-data",
        )
        .replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    let config = live_core::config::load_config(&config_path).expect("config loads");
    let built = live_core::demo::build_demo(&config, None).expect("demo builds");
    let data_root = std::path::PathBuf::from(
        built["output_dir"]
            .as_str()
            .expect("demo reports output_dir"),
    );
    let app = build_app(AppState {
        config_path,
        web_root: tmp.path().join("no-dist"),
        registry: live_server::registry::Registry::new(),
        demo: true,
        data_root: Some(data_root.clone()),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
    });
    Fixture {
        _tmp: tmp,
        app,
        data_root,
    }
}

async fn get(app: &axum::Router, path: &str) -> (u16, Value) {
    let request = Request::builder()
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test(flavor = "multi_thread")]
async fn overview_combines_collection_ai_leads_and_baseline_delta() {
    let fx = fixture();
    let (status, body) = get(&fx.app, "/api/rooms/983/overview").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["room_id"], "983");
    assert_eq!(body["collection"]["status"], "complete");
    // M4.x-T1：缺键显式 i64 0
    assert_eq!(body["collection"]["leads_consumed"], 0);
    assert_eq!(body["ai"]["status"], "complete");
    // 线索簿：demo 合成通道不带 leads → 空集合与空 summary
    assert_eq!(body["leads"]["totals"]["pending_approval"], 0);
    assert!(body["leads"]["pending"].is_array());
    // G4：单次 complete run → 基线态（前端显示「基线已建」）
    assert_eq!(
        body["delta"]["baseline_only"], true,
        "单次 complete 观众的 delta 码必须是基线态：{}",
        body["delta"]
    );
    // Z2：主播卡/直播档案两新面必须始终就位（demo 布景无 profile/records → null 空态，
    // 而不是缺键——前端空态分支靠「键存在且值为 null」判别，缺键会让判别歧义）。
    let keys = body.as_object().expect("overview object");
    assert!(keys.contains_key("streamer"), "{body}");
    assert!(keys.contains_key("live"), "{body}");
    assert!(body["streamer"].is_null(), "{body}");
    assert!(body["live"].is_null(), "{body}");
    // 未知 uid → 404
    let (status, body) = get(&fx.app, "/api/rooms/999/overview").await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn overview_passes_through_streamer_profile_and_live_records() {
    let fx = fixture();
    // Z2 主播卡面：streamer.json 的 profile 段原样透传——sources（原始事实原料，体大）
    // 不得爬上 overview 面，由 Vue/直播数据页之外的专用端点或文件面另行服务。
    live_core::storage::write_json(
        &fx.data_root.join("streamer.json"),
        &serde_json::json!({
            "profile": {
                "uid": "u-1",
                "name": "演示主播",
                "face": "https://i1.hdslb.com/bfs/face/demo.jpg",
            },
            "sources": { "videos": [ { "id": "v-big-blob" } ] },
        }),
    )
    .expect("streamer fixture writes");
    live_core::storage::write_json(
        &fx.data_root.join("shared").join("live_records.json"),
        &serde_json::json!({
            "platform": "bilibili",
            "status": "ok",
            "count": 1,
            "records": [ { "title": "第N场", "start_time": 1754208000 } ],
        }),
    )
    .expect("live fixture writes");

    let (status, body) = get(&fx.app, "/api/rooms/983/overview").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["streamer"]["name"], "演示主播", "{body}");
    assert!(
        body["streamer"].get("sources").is_none(),
        "sources/videos 原料段不得上 overview 面：{body}"
    );
    assert_eq!(body["live"]["status"], "ok", "{body}");
    assert_eq!(body["live"]["records"][0]["title"], "第N场", "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn viewers_list_reports_ai_completion_per_viewer() {
    let fx = fixture();
    let (status, body) = get(&fx.app, "/api/rooms/983/viewers").await;
    assert_eq!(status, 200, "{body}");
    let viewers = body.as_array().expect("list");
    assert_eq!(viewers.len(), 3, "演示三观众：{body}");
    assert!(
        viewers
            .iter()
            .all(|v| v["ai_completed"] == true && v["ai_status"] == "complete"),
        "{body}"
    );
    assert!(viewers.iter().any(|v| v["name"] == "演示观众A"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_tree_joins_raw_ai_episodes_mentions() {
    let fx = fixture();
    let (status, body) = get(&fx.app, "/api/rooms/983/viewers/demo-1/tree").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["uid"], "demo-1");
    assert_eq!(body["viewer"]["schema_version"], 1);
    assert_eq!(body["ai"]["status"], "complete");
    // episodes：合成通道至少一条（demo 造态是真实查询面）
    assert!(
        body["episodes"].as_array().is_some_and(|e| !e.is_empty()),
        "{body}"
    );
    // mentions：合成通道至少一条带 proposed_entity_name
    let mentions = body["mentions"].as_array().expect("mentions");
    assert!(!mentions.is_empty(), "{body}");
    assert!(
        mentions[0]["proposed_entity_name"]
            .as_str()
            .is_some_and(|n| !n.is_empty()),
        "{body}"
    );
    // 不存在观众 → 404
    let (status, _body) = get(&fx.app, "/api/rooms/983/viewers/none-watch/tree").await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_endpoints_cytoscape_shape_and_viewer_scope() {
    let fx = fixture();
    let (status, body) = get(&fx.app, "/api/rooms/983/graph").await;
    assert_eq!(status, 200, "{body}");
    let elements = body["elements"].as_array().expect("elements");
    assert!(!elements.is_empty(), "{body}");
    // 形状钉：节点 data.id/label/kind；边 data.source/target/predicate
    assert!(
        elements
            .iter()
            .filter(|el| el["data"]["source"].is_null())
            .all(|el| el["data"]["id"].is_string() && el["data"]["kind"].is_string()),
        "{body}"
    );
    let edges: Vec<&Value> = elements
        .iter()
        .filter(|el| !el["data"]["source"].is_null())
        .collect();
    assert!(!edges.is_empty(), "{body}");
    assert!(
        edges.iter().all(|el| el["data"]["predicate"].is_string()),
        "{body}"
    );
    // id 完整性：每条边的 endpoint 存在于 nodes 集合中
    let node_ids: std::collections::BTreeSet<&str> = elements
        .iter()
        .filter(|el| el["data"]["source"].is_null())
        .filter_map(|el| el["data"]["id"].as_str())
        .collect();
    for edge in &edges {
        for key in ["source", "target"] {
            assert!(
                node_ids.contains(edge["data"][key].as_str().unwrap_or("")),
                "edge endpoint 必须在节点集：{edge}"
            );
        }
    }

    // viewer 作用域子图：每条边必须与 viewer:demo-1 相邻
    let (status, scoped) = get(&fx.app, "/api/rooms/983/viewers/demo-1/graph").await;
    assert_eq!(status, 200, "{scoped}");
    let scoped_elements = scoped["elements"].as_array().expect("scoped elements");
    let scoped_edges: Vec<&Value> = scoped_elements
        .iter()
        .filter(|el| !el["data"]["source"].is_null())
        .collect();
    assert!(!scoped_edges.is_empty(), "{scoped}");
    assert!(
        scoped_edges
            .iter()
            .all(|el| el["data"]["source"] == "viewer:demo-1"
                || el["data"]["target"] == "viewer:demo-1"),
        "{scoped}"
    );
    assert!(scoped_elements.len() < elements.len(), "子集必须收敛");
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_endpoints_404_when_graph_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("empty");
    std::fs::create_dir_all(&output_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "m5b-data",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "m5b-data",
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
        demo: false,
        data_root: None,
        bilibili_hosts: None,
        config_write_lock: Default::default(),
    });
    let (status, _body) = get(&app, "/api/rooms/983/graph").await;
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------------
// X1 修复批钉团（8-agent 盲评裁定）
// ---------------------------------------------------------------------------

/// ag2-F1/X1-a（blocker）：viewer 路径段的 vid 清洗——Path 抽取器把 %2F/%5C 解码成
/// 真斜杠，零校验时 tmp 之外的哨兵会经 viewers/../../ 穿透读出；guard 必须 404 截断。
#[tokio::test(flavor = "multi_thread")]
async fn viewer_tree_graph_reject_traversal_vids_404() {
    let fx = fixture();
    let long = "a".repeat(65); // 超 MAX_VID_PATH_CHARS=64
    for vid in [
        "..%2F..%2Fevidence-sink",
        "..%5C..%5Cevidence-sink",
        "with.dot",
        "with%20space",
        long.as_str(),
    ] {
        let (status, body) = get(&fx.app, &format!("/api/rooms/983/viewers/{vid}/tree")).await;
        assert_eq!(status, 404, "tree vid={vid} → {body}");
        assert!(
            body["error"].as_str().is_some_and(|s| s.contains("不存在")),
            "tree vid={vid} → {body}"
        );
        let (status, body) = get(&fx.app, &format!("/api/rooms/983/viewers/{vid}/graph")).await;
        assert_eq!(status, 404, "graph vid={vid} → {body}");
    }
    // 合法 demo vid 不得被误伤（正向对照）。
    let (status, _) = get(&fx.app, "/api/rooms/983/viewers/demo-1/tree").await;
    assert_eq!(status, 200, "demo-1 tree 应通行");
}
