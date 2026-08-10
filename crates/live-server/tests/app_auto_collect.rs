//! 自动采录开关端口钉团：POST /api/rooms/:uid/auto-collect
//! ——状态=ai/auto_collect.json 单文件（哨兵 Python 侧零依赖直读，
//! 不进图库不引 schema）。四钉——合法翻转落盘 / 幂等重放终态不变 /
//! 空体与坏参 422（显式拒）/ 未知房间 404。

use serde_json::{Value, json};
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
    std::fs::create_dir_all(&output_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "auto-collect",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "auto-collect",
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
        graph_artifact_lock: Default::default(),
    });
    Fixture {
        _tmp: tmp,
        app,
        data_root: output_dir,
    }
}

async fn post(app: &axum::Router, path: &str, body: Option<&str>) -> (u16, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.unwrap_or("").to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn read_switch(fx: &Fixture) -> Option<Value> {
    std::fs::read_to_string(fx.data_root.join("ai/auto_collect.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

#[tokio::test(flavor = "multi_thread")]
async fn toggle_writes_state_file_and_is_idempotent() {
    let fx = fixture();
    let (status, body) = post(
        &fx.app,
        "/api/rooms/983/auto-collect",
        Some(r#"{"enabled":true}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(body["changed"], json!(true));
    let first = read_switch(&fx).expect("开关文件落盘");
    assert_eq!(first["enabled"], json!(true));

    // 幂等重放：同终态 → changed=false，文件内容不动（updated_at 不漂移）。
    let (status, body) = post(
        &fx.app,
        "/api/rooms/983/auto-collect",
        Some(r#"{"enabled":true}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["changed"], json!(false));
    assert_eq!(
        read_switch(&fx).unwrap()["updated_at"],
        first["updated_at"],
        "同终态重放不得刷时间戳"
    );

    // 再转回 false → changed=true（旋钮真实回转）。
    let (status, body) = post(
        &fx.app,
        "/api/rooms/983/auto-collect",
        Some(r#"{"enabled":false}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["changed"], json!(true));
    assert_eq!(read_switch(&fx).unwrap()["enabled"], json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_body_and_non_bool_enabled_are_422() {
    let fx = fixture();
    let (status, _) = post(&fx.app, "/api/rooms/983/auto-collect", None).await;
    assert_eq!(status, 422, "空体显拒（静默放过坏参等于装肚里）");
    assert!(read_switch(&fx).is_none(), "422 不得落文件");

    let (status, _) = post(&fx.app, "/api/rooms/983/auto-collect", Some("not-json")).await;
    assert_eq!(status, 422);
    let (status, body) = post(
        &fx.app,
        "/api/rooms/983/auto-collect",
        Some(r#"{"enabled":"true"}"#),
    )
    .await;
    assert_eq!(status, 422, "enabled 非布尔显拒: {body}");
    let (status, body) = post(
        &fx.app,
        "/api/rooms/983/auto-collect",
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(status, 422, "缺 enabled 键显拒: {body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_room_gets_404() {
    let fx = fixture();
    let (status, _) = post(
        &fx.app,
        "/api/rooms/984/auto-collect",
        Some(r#"{"enabled":true}"#),
    )
    .await;
    assert_eq!(status, 404);
}
