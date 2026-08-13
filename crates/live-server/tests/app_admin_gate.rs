//! 写面口令闸钉团（2026-08-13 官规：线上触发 AI 更新须过密码）：
//! config.admin_token 非空 → 所有非 GET/HEAD /api/* 须携 x-admin-token 一致，
//! 否则 401 同形 {error}；GET 数据面恒公共只读；空串 → 不上锁（现状一字不动）。
//! 口令绝不经 /api/config 读面回显（白名单不转发，字节级不见真值）。

use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;

use live_server::app::{AppState, build_app};
use live_server::registry::Registry;

// ASCII 钉值——HTTP 头 to_str() 只认 visible ASCII（非 ASCII 头在闸内因
// to_str 失败视同未提供；真值口径见 app.rs admin_gate 注释）。
const TOKEN: &str = "swordfish-0426";

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
}

fn fixture(admin_token: Option<&str>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut text = common::yaml_template(
        None,
        "admin-gate",
        "SESSDATA=test",
        "test-key",
        "http://127.0.0.1:9/v1",
        "admin-gate",
    )
    .replace(
        "OUTPUT_DIR",
        &out_dir.display().to_string().replace('\\', "/"),
    );
    if let Some(token) = admin_token {
        use std::fmt::Write as _;
        writeln!(text, "admin_token: \"{token}\"").unwrap();
    }
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, text).unwrap();
    Fixture {
        _tmp: tmp,
        app: build_app(AppState {
            config_path,
            web_root: "no-dist".into(),
            registry: Registry::new(),
            bilibili_hosts: None,
            graph_artifact_lock: Default::default(),
        }),
    }
}

async fn oneshot(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body
        .map(|value| axum::body::Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap_or_else(axum::body::Body::empty);
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("x-admin-token", token);
    }
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn locked_writes_401_without_or_with_wrong_token() {
    let fx = fixture(Some(TOKEN));
    let app = &fx.app;
    // 无头 → 401。
    let (status, body) = oneshot(
        app,
        "POST",
        "/api/runs",
        None,
        Some(json!({"kind": "bogus"})),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(
        body["error"].as_str().unwrap(),
        "写面已上锁：请提供 x-admin-token 管理口令"
    );
    // 错值 → 401（真值绝不进错误文案）。
    let (status, body) = oneshot(
        app,
        "POST",
        "/api/runs",
        Some("wrong"),
        Some(json!({"kind": "bogus"})),
    )
    .await;
    assert_eq!(status, 401);
    assert!(!body.to_string().contains(TOKEN));
}

#[tokio::test]
async fn locked_writes_pass_with_right_token() {
    let fx = fixture(Some(TOKEN));
    let app = &fx.app;
    // 闸后抵达 handler 的证明 = kind 校验原生 422（闸若误拦则只会见 401）。
    let (status, _body) = oneshot(
        app,
        "POST",
        "/api/runs",
        Some(TOKEN),
        Some(json!({"kind": "bogus"})),
    )
    .await;
    assert_eq!(status, 422, "{_body}");
}

#[tokio::test]
async fn reads_stay_open_when_locked() {
    let fx = fixture(Some(TOKEN));
    let app = &fx.app;
    let (status, _body) = oneshot(app, "GET", "/api/rooms", None, None).await;
    assert_eq!(status, 200, "{_body}");
}

#[tokio::test]
async fn unlocked_when_token_absent() {
    let fx = fixture(None);
    let app = &fx.app;
    // 不上锁：无头写直达 handler（同 422 证道）。
    let (status, _body) = oneshot(
        app,
        "POST",
        "/api/runs",
        None,
        Some(json!({"kind": "bogus"})),
    )
    .await;
    assert_eq!(status, 422, "{_body}");
}

#[tokio::test]
async fn config_read_face_never_echoes_token() {
    let fx = fixture(Some(TOKEN));
    let app = &fx.app;
    let (status, body) = oneshot(app, "GET", "/api/config", None, None).await;
    assert_eq!(status, 200, "{body}");
    assert!(!body.to_string().contains(TOKEN));
}
