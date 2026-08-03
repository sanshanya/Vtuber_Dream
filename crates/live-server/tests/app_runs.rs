//! M5-B3b runs 通道钉团：POST 校验 + demo 静态快照幂等 + spawn 生命周期（G3/D3）。
//!
//! 全部通路走 `build_app` + tower ServiceExt（不起真端口）。spawn 生命周期用
//! wiremock 作 Bilibili 根地址注入（AppState.bilibili_hosts seam）——404 即证明
//! 流量已被海洋洞穿，不做真实外呼。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use live_server::app::{AppState, MAX_VIEWER_UID_CHARS, build_app};
use live_server::registry::{Registry, RunRecord};

const YAML_TEMPLATE: &str = r#"
version: 6
project:
  name: m5b-runs
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
  model: m5b-runs
  reasoning:
    enabled: false
  agent:
    max_turns: 4
    run_retries: 0
  max_output_tokens: 131072
report:
  title: t
"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    registry: Registry,
}

fn fixture(demo: bool, bilibili_hosts: Option<(String, String)>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(
        &config_path,
        YAML_TEMPLATE.replace(
            "OUTPUT_DIR",
            &out_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    let registry = Registry::new();
    let app = build_app(AppState {
        config_path,
        web_root: tmp.path().join("no-dist"),
        registry: registry.clone(),
        demo,
        data_root: None,
        bilibili_hosts,
    });
    Fixture {
        _tmp: tmp,
        app,
        registry,
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
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn record_json(record: &Arc<Mutex<RunRecord>>) -> Value {
    live_server::registry::run_to_json(&record.lock().expect("record poisoned"))
}

// ---------------------------------------------------------------------------
// POST /api/runs 校验电池（D3/D9）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn runs_post_validation_battery_422() {
    let fx = fixture(false, None);
    let cases: Vec<Value> = vec![
        json!(42),                                                     // 非对象
        json!({}),                                                     // 缺 kind
        json!({"kind": "survey"}),                                     // 未知 kind
        json!({"kind": 1}),                                            // kind 非字符串
        json!({"kind": "full", "force": "yes"}),                       // force 非布尔
        json!({"kind": "viewer"}),                                     // viewer 缺 uid
        json!({"kind": "viewer", "viewer_uid": "  "}),                 // uid 空白
        json!({"kind": "viewer", "viewer_uid": 42}),                   // uid 非字符串
        json!({"kind": "viewer", "viewer_uid": "123", "force": true}), // viewer+force 互斥
        json!({"kind": "full", "viewer_uid": "123"}),                  // full 不带 uid
        json!({"kind": "viewer", "viewer_uid": "1".repeat(MAX_VIEWER_UID_CHARS + 1)}),
    ];
    for case in &cases {
        let (status, body) = oneshot(&fx.app, "POST", "/api/runs", Some(case.clone())).await;
        assert_eq!(status, 422, "case {case} → {body}");
        assert!(
            body["error"].as_str().is_some_and(|s| !s.is_empty()),
            "{body}"
        );
    }
    // 校验电池不得有副作用：一个 run 都没登记。
    assert!(fx.registry.get("nope").is_none());
}

// ---------------------------------------------------------------------------
// demo 模式：静态快照幂等（G3）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn demo_runs_post_returns_idempotent_snapshot() {
    let fx = fixture(true, None);
    let (status_a, body_a) =
        oneshot(&fx.app, "POST", "/api/runs", Some(json!({"kind": "full"}))).await;
    assert_eq!(status_a, 202, "{body_a}");
    let run_id = body_a["run_id"].as_str().expect("run_id 字符串");
    let (status_b, body_b) = oneshot(
        &fx.app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "viewer", "viewer_uid": "77"})),
    )
    .await;
    assert_eq!(status_b, 202, "{body_b}");
    // G3：重复 POST 返回同一静态快照，不触发真实运行。
    assert_eq!(body_b["run_id"].as_str(), Some(run_id));

    let (status, snapshot) = oneshot(&fx.app, "GET", &format!("/api/runs/{run_id}"), None).await;
    assert_eq!(status, 200, "{snapshot}");
    assert_eq!(snapshot["status"], "done");
    assert_eq!(snapshot["partial"], false);
    assert_eq!(snapshot["outcome"]["synthetic_demo"], true);
    assert!(snapshot["finished_at"].is_string(), "{snapshot}");
    assert_eq!(snapshot["events"], json!([]), "快照无 progress 事件");
}

// ---------------------------------------------------------------------------
// spawn 生命周期：collect 渠道真实启动 → wiremock 404 → failed（确定性、无外呼）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn spawn_run_full_lifecycle_collect_failure_marks_failed() {
    let mock = wiremock::MockServer::start().await;
    let fx = fixture(false, Some((mock.uri(), mock.uri())));

    let (status, body) = oneshot(&fx.app, "POST", "/api/runs", Some(json!({"kind": "full"}))).await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().expect("run_id 字符串").to_string();

    // held registry handle：直接等终态，不靠 HTTP 轮询时序。
    let record = fx.registry.get(&run_id).expect("run registered");
    let deadline = Instant::now() + Duration::from_secs(60);
    let terminal = loop {
        let snapshot = record_json(&record);
        if ["done", "failed"].contains(&snapshot["status"].as_str().unwrap_or("")) {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "run 未在时限内到达终态：{snapshot}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(terminal["status"], "failed", "{terminal}");
    assert_eq!(terminal["partial"], false, "未走到 viewer 阶段：{terminal}");
    assert!(terminal["finished_at"].is_string(), "{terminal}");
    let error = terminal["outcome"]["error"]
        .as_str()
        .expect("failed run 必须带 error 体");
    assert!(!error.is_empty());
    let events = terminal["events"].as_array().expect("events 数组");
    assert!(
        events
            .iter()
            .any(|line| line.as_str().unwrap_or("").contains("触发 kind=full")),
        "events 须留 spawn 足迹：{events:?}"
    );

    // seam 证真：流量确实打到注入的 wiremock（而非官方端点）。
    let received = mock.received_requests().await.unwrap_or_default();
    assert!(!received.is_empty(), "bilibili_hosts seam 未接住任何请求");

    // GET /api/runs/{id} 回放同一终态（D3 轮询载体同形）。
    let (status, via_http) = oneshot(&fx.app, "GET", &format!("/api/runs/{run_id}"), None).await;
    assert_eq!(status, 200, "{via_http}");
    assert_eq!(via_http["status"], "failed");
    assert_eq!(via_http["kind"], "full");
    assert_eq!(via_http["viewer_uid"], Value::Null);
}
