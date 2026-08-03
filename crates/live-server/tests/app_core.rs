//! M5-B1 oneshot 钉团：rooms + config 打码/写入 + 静态面 fallback。
//!
//! 全部通路走 `build_app` + tower ServiceExt（不起真端口；真端口 smoke 另 env-gated）。

use std::path::PathBuf;

use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use live_server::app::{AppState, build_app};

const YAML_TEMPLATE: &str = r#"
# 本行是用户的注释——PUT 后不得丢。
version: 6
project:
  name: m5b-app
  output_dir: OUTPUT_DIR
bilibili:
  room_id: "983"
  streamer_uid: "9001"
  cookie: "SESSDATA=supersecret"   # 绝不回显
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
  api_key: supersecret-key
  model: m5b-app
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
    config_path: PathBuf,
}

fn fixture(web_root: Option<&str>) -> Fixture {
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
    let web_root = tmp.path().join(web_root.unwrap_or("no-dist"));
    let registry = live_server::registry::Registry::new();
    let app = build_app(AppState {
        config_path: config_path.clone(),
        web_root,
        registry,
        demo: false,
    });
    Fixture {
        _tmp: tmp,
        app,
        config_path,
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

#[tokio::test(flavor = "multi_thread")]
async fn rooms_list_reflects_config_and_masks_nothing_extra() {
    let fx = fixture(None);
    let (status, body) = oneshot(&fx.app, "GET", "/api/rooms", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body[0]["id"], "983");
    assert_eq!(body[0]["streamer_uid"], "9001");
    assert_eq!(body[0]["project_name"], "m5b-app");
}

#[tokio::test(flavor = "multi_thread")]
async fn config_get_never_echoes_secrets() {
    let fx = fixture(None);
    let (status, body) = oneshot(&fx.app, "GET", "/api/config", None).await;
    assert_eq!(status, 200, "{body}");
    let rendered = serde_json::to_string(&body).unwrap();
    assert!(!rendered.contains("supersecret"), "{rendered}");
    assert_eq!(body["bilibili"]["cookie_present"], true);
    assert_eq!(body["ai"]["api_key_present"], true);
    assert_eq!(body["ai"]["base_url"], "http://127.0.0.1:9/v1");
    assert_eq!(body["ai"]["model"], "m5b-app");
}

#[tokio::test(flavor = "multi_thread")]
async fn config_put_rewrites_whitelisted_keys_in_place_and_preserves_comments() {
    let fx = fixture(None);
    // 白名单替换 + 值带空格（quoted 必现）+ 空串保持
    let (status, body) = oneshot(
        &fx.app,
        "PUT",
        "/api/config",
        Some(json!({
            "bilibili": {"cookie": "SESSDATA=new、中文", "extra_added": "悄入"},
            "ai": {"api_key": "", "model": "deepseek-v3 qwen α"},
        })),
    )
    .await;
    assert_eq!(status, 422, "{body}"); // extra_added 不在白名单 → 整体拒
    // 原文件不动
    let original = std::fs::read_to_string(&fx.config_path).unwrap();
    assert!(
        original.contains("SESSDATA=supersecret"),
        "拒绝时不得落盘：{original}"
    );

    let (status, body) = oneshot(
        &fx.app,
        "PUT",
        "/api/config",
        Some(json!({
            "bilibili": {"cookie": "SESSDATA=new、中文"},
            "ai": {"api_key": "", "model": "deepseek-v3 qwen α"},
        })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "updated");
    assert_eq!(
        body["keys"], 2,
        "空串 cookie 替换为 1，api_key 空串保持，model 替换为 1"
    );
    let written = std::fs::read_to_string(&fx.config_path).unwrap();
    assert!(
        written.starts_with("\n# 本行是用户的注释"),
        "注释保留前置行：{written}"
    );
    assert!(!written.contains("SESSDATA=supersecret"), "{written}");
    // reload 得到新值
    let (status, body) = oneshot(&fx.app, "GET", "/api/config", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ai"]["model"], "deepseek-v3 qwen α");
    let rendered = serde_json::to_string(&body).unwrap();
    assert!(!rendered.contains("new、中文"), "新 cookie 也不回显");
}

#[tokio::test(flavor = "multi_thread")]
async fn put_with_only_blank_values_is_unchanged_noop() {
    let fx = fixture(None);
    let original = std::fs::read_to_string(&fx.config_path).unwrap();
    let (status, body) = oneshot(
        &fx.app,
        "PUT",
        "/api/config",
        Some(json!({"bilibili": {"cookie": "  "}, "ai": {"api_key": ""}})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "unchanged");
    assert_eq!(std::fs::read_to_string(&fx.config_path).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_dist_returns_build_guide_not_silence() {
    let fx = fixture(None);
    let request = Request::builder()
        .uri("/")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = fx.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let text = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("前端尚未构建"), "{text}");
    assert!(text.contains("npm run build"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_dist_serves_index() {
    let _tmp = tempfile::tempdir().unwrap();
    let dist = _tmp.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    // dist 判定发生在 build_app；文件须先于装配存在。
    std::fs::write(dist.join("index.html"), "<h1>dist-ok</h1>").unwrap();
    let config_path = _tmp.path().join("config.yaml");
    std::fs::write(&config_path, YAML_TEMPLATE.replace("OUTPUT_DIR", "./out")).unwrap();
    let app = build_app(AppState {
        config_path,
        web_root: dist,
        registry: live_server::registry::Registry::new(),
        demo: false,
    });
    let request = Request::builder()
        .uri("/")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let text = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("dist-ok"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_run_id_is_404() {
    let fx = fixture(None);
    let (status, body) = oneshot(&fx.app, "GET", "/api/runs/nope", None).await;
    assert_eq!(status, 404, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("nope"),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn endpoints_marked_for_b2_b3_respond_not_implemented_yet() {
    let fx = fixture(None);
    let (status, _) = oneshot(&fx.app, "GET", "/api/rooms/983/overview", None).await;
    assert_eq!(status, 501);
    let (status, _) = oneshot(&fx.app, "POST", "/api/runs", Some(json!({"kind": "full"}))).await;
    assert_eq!(status, 501);
}
