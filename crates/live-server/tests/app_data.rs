//! M5-B2 数据面钉团：overview（含 delta/leads 区块）/viewers/tree/graph（cytoscape DTO）。
//!
//! 布景 = build_demo 到临时输出根 → web/dist 无参与（fallback 另案钉）。

use std::io::Read as _;

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
    config_path: std::path::PathBuf,
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
        config_path: config_path.clone(),
        web_root: tmp.path().join("no-dist"),
        registry: live_server::registry::Registry::new(),
        demo: true,
        data_root: Some(data_root.clone()),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });
    Fixture {
        _tmp: tmp,
        app,
        data_root,
        config_path,
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
    // Z3：首页指标条 graph_stats（demo 布景图存在 → 非 null 且 episodes/entities ≥ 1）。
    let stats = &body["graph_stats"];
    assert!(stats.is_object(), "demo 布景有图 → 指标面必须建账：{body}");
    assert!(stats["episodes"].as_i64().unwrap_or(0) > 0, "{stats}");
    assert!(stats["entities"].as_i64().unwrap_or(0) > 0, "{stats}");
    assert!(stats.get("relations").is_some() && stats.get("interest_states").is_some());
    // Z5/C1：BriefingCard refs 归属解析面——episode_index 恒在（键纪律同上），
    // demo 布景有图 → 非空且每行恰两键 viewer_id + title（大键不上 overview 面）。
    let index = &body["episode_index"];
    let entries = index.as_object().expect("episode_index 是对象");
    assert!(
        !entries.is_empty(),
        "demo 布景有 episodes → 索引非空：{body}"
    );
    for (episode_id, entry) in entries {
        assert!(
            entry["viewer_id"].as_str().is_some_and(|v| !v.is_empty()),
            "{episode_id}: {entry}"
        );
        assert!(
            entry
                .as_object()
                .unwrap()
                .keys()
                .all(|key| key == "viewer_id" || key == "title"),
            "索引行只许 viewer_id/title 两键：{entry}"
        );
    }
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
    // Z3：大航海身份面透传——face/guard_level/medal_level 键恒在（demo face 空串 = 未采到，
    // 呈现侧 fallback；guard/medal 是有意义数字必须原样到面）。
    for row in viewers {
        let obj = row.as_object().expect("viewer row object");
        for key in ["face", "guard_level", "medal_level"] {
            assert!(obj.contains_key(key), "观众行缺 {key}：{row}");
        }
    }
    assert_eq!(viewers[0]["guard_level"], 3, "{body}");
    assert_eq!(viewers[0]["medal_level"], 20, "{body}");
}

/// Z5c 时效位：旧 AI 结论保留不删，但 fact 面/提示面已变 → 行面 ai_stale 必须亮
/// 「信源已更新」；哈希同参镇定 → stale=false；三态互斥互不串扰。
#[tokio::test(flavor = "multi_thread")]
async fn viewers_list_marks_stale_perception_hash_flips() {
    let fx = fixture();
    let config =
        live_core::config::load_config(fx._tmp.path().join("config.yaml")).expect("config loads");

    let write_cache = |uid: &str, hash: &str| {
        let cached = serde_json::json!({
            "status": "complete",
            "input_hash": hash,
            "analysis": {"profile_summary": "x"},
        });
        let path = fx
            .data_root
            .join("ai")
            .join("perception")
            .join("viewers")
            .join(format!("{uid}.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&cached).unwrap()).unwrap();
    };
    let current_hash_of = |uid: &str| -> String {
        let raw = live_core::storage::read_json(
            &fx.data_root.join("viewers").join(format!("{uid}.json")),
        )
        .expect("viewer reads")
        .expect("viewer exists");
        let profile = live_core::episodes::baseline::viewer_input(
            &raw,
            config.perception.max_evidence_per_viewer as usize,
        );
        let reasoning = serde_json::json!({
            "enabled": config.ai.reasoning.enabled,
            "effort": config.ai.reasoning.effort.clone(),
            "replay_content": config.ai.reasoning.replay_content,
        });
        let bundle = live_core::agent::pipeline::viewer_input_bundle(
            &raw,
            &profile,
            &config.ai.model,
            &config.ai.api,
            &reasoning,
            &config.ai.rules,
            config.perception.max_evidence_per_viewer as usize,
        );
        bundle.input_hash
    };

    // demo-2 码现行哈希 → 绿灯；demo-1 栽错哈希 → 必亮过期
    write_cache("demo-2", &current_hash_of("demo-2"));
    write_cache("demo-1", "deadbeef");
    let (status, body) = get(&fx.app, "/api/rooms/983/viewers").await;
    assert_eq!(status, 200, "{body}");
    let rows = body.as_array().unwrap();
    let demo1 = rows.iter().find(|v| v["uid"] == "demo-1").expect("demo-1");
    let demo2 = rows.iter().find(|v| v["uid"] == "demo-2").expect("demo-2");
    assert_eq!(
        demo1["ai_stale"],
        Value::Bool(true),
        "哈希翻 → 时效位必须亮：{demo1}"
    );
    assert_eq!(
        demo2["ai_stale"],
        Value::Bool(false),
        "哈希稳定 → 时效位绿灯：{demo2}"
    );

    // demo-1 补码现行哈希 → 过期位应熄灭（时效位跟据哈希实况，不是一次性旗帜）
    write_cache("demo-1", &current_hash_of("demo-1"));
    let (status, body) = get(&fx.app, "/api/rooms/983/viewers").await;
    assert_eq!(status, 200, "{body}");
    let demo1 = body
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["uid"] == "demo-1")
        .expect("demo-1");
    assert_eq!(
        demo1["ai_stale"],
        Value::Bool(false),
        "哈希复位 → 时效位熄灭：{demo1}"
    );
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

/// Z5c 时效位的 tree 面钉：与 room_viewers 行同源算法（同 config/raw/cached 三参）。
/// 栽现行哈希 → false；栽死哈希 → true；哈希复位 → 熄灭。
#[tokio::test(flavor = "multi_thread")]
async fn viewer_tree_marks_stale_perception_hash_flips() {
    let fx = fixture();
    let config =
        live_core::config::load_config(fx._tmp.path().join("config.yaml")).expect("config loads");

    let write_cache = |uid: &str, hash: &str| {
        let cached = serde_json::json!({
            "status": "complete",
            "input_hash": hash,
            "analysis": {"profile_summary": "x"},
        });
        let path = fx
            .data_root
            .join("ai")
            .join("perception")
            .join("viewers")
            .join(format!("{uid}.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&cached).unwrap()).unwrap();
    };
    let current_hash_of = |uid: &str| -> String {
        let raw = live_core::storage::read_json(
            &fx.data_root.join("viewers").join(format!("{uid}.json")),
        )
        .expect("viewer reads")
        .expect("viewer exists");
        let profile = live_core::episodes::baseline::viewer_input(
            &raw,
            config.perception.max_evidence_per_viewer as usize,
        );
        let reasoning = serde_json::json!({
            "enabled": config.ai.reasoning.enabled,
            "effort": config.ai.reasoning.effort.clone(),
            "replay_content": config.ai.reasoning.replay_content,
        });
        let bundle = live_core::agent::pipeline::viewer_input_bundle(
            &raw,
            &profile,
            &config.ai.model,
            &config.ai.api,
            &reasoning,
            &config.ai.rules,
            config.perception.max_evidence_per_viewer as usize,
        );
        bundle.input_hash
    };

    // demo-2 栽现行哈希 → 绿灯；demo-1 栽死哈希 → 必亮过期。
    write_cache("demo-2", &current_hash_of("demo-2"));
    write_cache("demo-1", "deadbeef");
    let (status, body) = get(&fx.app, "/api/rooms/983/viewers/demo-1/tree").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["ai_stale"],
        Value::Bool(true),
        "tree 面哈希翻 → 时效位必须亮：{body}"
    );
    let (status, body) = get(&fx.app, "/api/rooms/983/viewers/demo-2/tree").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["ai_stale"],
        Value::Bool(false),
        "tree 面哈希稳定 → 时效位绿灯：{body}"
    );

    // demo-1 补码现行哈希 → 过期位应熄灭（时效位跟据哈希实况，不是一次性旗帜）。
    write_cache("demo-1", &current_hash_of("demo-1"));
    let (status, body) = get(&fx.app, "/api/rooms/983/viewers/demo-1/tree").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["ai_stale"],
        Value::Bool(false),
        "tree 面哈希复位 → 时效位熄灭：{body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_endpoints_cytoscape_shape_and_viewer_scope() {
    let fx = fixture();
    // Z6：默认视图已折叠——「全量」形状钉走显式 all 逃生门（Z6 前 payload 面同形）。
    let (status, body) = get(&fx.app, "/api/rooms/983/graph?kinds=all").await;
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

// ---------------------------------------------------------------------------
// Z6/P0-6：整体图谱物化协商 + kind 折叠钉团
// ---------------------------------------------------------------------------

/// 带请求头的原始往返（status, headers, raw bytes）。
async fn request_raw(
    app: &axum::Router,
    path: &str,
    headers: &[(&str, &str)],
) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().uri(path);
    for (key, value) in headers {
        builder = builder.header(*key, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let hdrs = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, hdrs, bytes.to_vec())
}

/// 默认视图 = 配置白名单折叠投影；物化协商面：CE/ETag/Vary/Cache-Control + 304。
#[tokio::test(flavor = "multi_thread")]
async fn graph_default_folded_serves_artifact_with_etag_gzip_and_304() {
    let fx = fixture();
    // 1) 协商顺序钉：br 优先 → CE=br 且可 brotli 解码；gzip 请求 → CE=gzip 且可解压。
    let (status, hdrs, bytes) = request_raw(
        &fx.app,
        "/api/rooms/983/graph",
        &[("accept-encoding", "br, gzip")],
    )
    .await;
    assert_eq!(status.as_u16(), 200, "headers={hdrs:?}");
    assert_eq!(hdrs.get("content-encoding").unwrap(), "br", "br 优先");
    let mut decompressed = Vec::new();
    brotli::Decompressor::new(&bytes[..], 4096)
        .read_to_end(&mut decompressed)
        .expect("br 体可解码");
    let etag = hdrs
        .get("etag")
        .expect("etag header")
        .to_str()
        .unwrap()
        .to_string();
    assert!(etag.starts_with('\"') && etag.len() > 10, "{etag}");
    assert!(
        hdrs.get("vary")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Accept-Encoding")
    );
    assert_eq!(hdrs.get("cache-control").unwrap(), "no-cache");

    let (status_g, hdrs_g, bytes_g) = request_raw(
        &fx.app,
        "/api/rooms/983/graph",
        &[("accept-encoding", "gzip")],
    )
    .await;
    assert_eq!(status_g.as_u16(), 200);
    assert_eq!(hdrs_g.get("content-encoding").unwrap(), "gzip");
    assert_eq!(
        hdrs_g.get("etag").unwrap(),
        &etag,
        "ETag 与编码无关（内容寻址）"
    );
    let mut plain = String::new();
    flate2::read::GzDecoder::new(&bytes_g[..])
        .read_to_string(&mut plain)
        .expect("gzip 体可解压");
    assert_eq!(
        plain.as_bytes(),
        decompressed.as_slice(),
        "两编码解码后必须同体"
    );
    let body: Value = serde_json::from_str(&plain).expect("解压体是 JSON");
    let elements = body["elements"].as_array().expect("elements");
    // 2) 折叠断言：demo 布景全谱含 Episode/Mention——默认视图必须折叠殆尽；
    //    存活边双端都在投影内（悬空裁除完整性）。
    let kinds: std::collections::BTreeSet<&str> = elements
        .iter()
        .filter_map(|el| el["data"]["kind"].as_str())
        .collect();
    assert!(
        kinds.iter().all(|kind| ["Viewer", "Entity"].contains(kind)),
        "默认视图 kind 收规：{kinds:?}"
    );
    let node_ids: std::collections::BTreeSet<&str> = elements
        .iter()
        .filter(|el| el["data"]["kind"].is_string())
        .filter_map(|el| el["data"]["id"].as_str())
        .collect();
    let edges: Vec<&Value> = elements
        .iter()
        .filter(|el| !el["data"]["source"].is_null())
        .collect();
    assert!(!edges.is_empty(), "默认视图必须保有 INTERESTED_IN：{body}");
    for edge in &edges {
        for key in ["source", "target"] {
            assert!(
                node_ids.contains(edge["data"][key].as_str().unwrap_or("")),
                "悬空边必须裁除：{edge}"
            );
        }
    }
    // 3) 304：If-None-Match 命中 → 空体 + 同 ETag。
    let (status_304, hdrs_304, bytes_304) = request_raw(
        &fx.app,
        "/api/rooms/983/graph",
        &[
            ("accept-encoding", "gzip"),
            ("if-none-match", etag.as_str()),
        ],
    )
    .await;
    assert_eq!(status_304, axum::http::StatusCode::NOT_MODIFIED);
    assert_eq!(hdrs_304.get("etag").unwrap(), &etag);
    assert!(bytes_304.is_empty(), "304 不能有体");

    // 4) 物化件三通道按内容寻址 etag 命名落盘（demo 布景折叠体 > 压缩阈值 → trio 齐）。
    let store_dir = fx.data_root.join("graph");
    let bare_etag = etag.trim_matches('\"');
    for path in [
        format!("web-graph.{bare_etag}.json"),
        format!("web-graph.{bare_etag}.json.gz"),
        format!("web-graph.{bare_etag}.json.br"),
    ] {
        assert!(store_dir.join(&path).exists(), "缺物化档 {path}");
    }
}

/// ?kinds=all 逃生门 = Z6 前的全量面；csv 自定义折叠现算直通；未知类响亮 400。
#[tokio::test(flavor = "multi_thread")]
async fn graph_kinds_all_escape_and_csv_straight_through_and_bad_kind_400() {
    let fx = fixture();
    let (status_all, _, bytes_all) =
        request_raw(&fx.app, "/api/rooms/983/graph?kinds=all", &[]).await;
    assert_eq!(status_all, 200);
    let all: Value = serde_json::from_slice(&bytes_all).unwrap();
    let all_kinds: std::collections::BTreeSet<&str> = all["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|el| el["data"]["kind"].as_str())
        .collect();
    assert!(
        all_kinds.contains("Episode") && all_kinds.contains("Mention"),
        "all 逃生门必须含细节层：{all_kinds:?}"
    );

    let (status_v, _, bytes_v) =
        request_raw(&fx.app, "/api/rooms/983/graph?kinds=Viewer", &[]).await;
    assert_eq!(status_v, 200);
    let only_viewers: Value = serde_json::from_slice(&bytes_v).unwrap();
    let list = only_viewers["elements"].as_array().unwrap();
    assert!(!list.is_empty());
    assert!(
        list.iter().all(|el| el["data"]["kind"] == "Viewer"),
        "{only_viewers}"
    );

    let (status_bad, _, bytes_bad) =
        request_raw(&fx.app, "/api/rooms/983/graph?kinds=Viewer,Unicorn", &[]).await;
    assert_eq!(status_bad, 400);
    let err: Value = serde_json::from_slice(&bytes_bad).unwrap();
    let message = err["error"].as_str().unwrap();
    assert!(
        message.contains("Unicorn") && message.contains("Viewer/Entity"),
        "{err}"
    );
}

/// 失效面一：配置白名单改动（重写 yaml；app 每请求 load_config）→ 指纹 kinds 键
/// 翻面 → 重建 + 新默认视图构图随配置走。
#[tokio::test(flavor = "multi_thread")]
async fn graph_artifact_rebuilds_when_whitelist_config_changes() {
    let fx = fixture();
    let (_, hdrs, _) = request_raw(&fx.app, "/api/rooms/983/graph", &[]).await;
    let etag_before = hdrs.get("etag").unwrap().to_str().unwrap().to_string();

    let yaml = std::fs::read_to_string(&fx.config_path).unwrap();
    std::fs::write(
        &fx.config_path,
        yaml.replace(
            "  peer_discovery:",
            "  graph_default_expanded_kinds: [Viewer]\n  peer_discovery:",
        ),
    )
    .unwrap();
    let (status, hdrs2, bytes) = request_raw(&fx.app, "/api/rooms/983/graph", &[]).await;
    assert_eq!(status, 200);
    let etag_after = hdrs2.get("etag").unwrap().to_str().unwrap();
    assert_ne!(etag_before, etag_after, "白名单翻面必须翻面 ETag");
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let kinds: std::collections::BTreeSet<&str> = body["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|el| el["data"]["kind"].as_str())
        .collect();
    assert_eq!(kinds, ["Viewer"].into_iter().collect(), "{body}");
}

/// 失效面二：图库写入新节点 → (mtime,len) 指纹翻面 → 重建；
/// ETag 内容寻址：内容变 → ETag 必须变。
#[tokio::test(flavor = "multi_thread")]
async fn graph_artifact_etag_follows_store_content() {
    let fx = fixture();
    let (_, hdrs, _) = request_raw(&fx.app, "/api/rooms/983/graph", &[]).await;
    let etag_before = hdrs.get("etag").unwrap().to_str().unwrap().to_string();

    let conn = rusqlite::Connection::open(fx.data_root.join("graph/perception.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO nodes (node_id, node_type, name, properties_json, source_kind, first_seen_at, last_seen_at) \
         VALUES ('viewer:z6-probe', 'Viewer', 'Z6探针', '{}', 'platform', 's', 's')",
        [],
    )
    .unwrap();
    drop(conn);

    let (status, hdrs2, bytes) = request_raw(&fx.app, "/api/rooms/983/graph", &[]).await;
    assert_eq!(status, 200);
    let etag_after = hdrs2.get("etag").unwrap().to_str().unwrap();
    assert_ne!(etag_before, etag_after, "内容变 → 内容寻址 ETag 必须变");
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["elements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|el| el["data"]["id"] == "viewer:z6-probe"),
        "新节点必须进默认视图（Viewer 在白名单）：{body}"
    );
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
        graph_artifact_lock: Default::default(),
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
