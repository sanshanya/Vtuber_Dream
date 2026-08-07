//! M5-B1 oneshot 钉团：rooms + config 打码/写入 + 静态面 fallback。
//!
//! 全部通路走 `build_app` + tower ServiceExt（不起真端口；真端口 smoke 另 env-gated）。

use std::path::PathBuf;

use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use live_server::app::{AppState, build_app};

mod common;

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
        common::yaml_template(
            Some("# 本行是用户的注释——PUT 后不得丢。"),
            "m5b-app",
            "SESSDATA=supersecret",
            "supersecret-key",
            "http://127.0.0.1:9/v1",
            "m5b-app",
        )
        .replace(
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
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
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

/// 拼错键+空串不得静默绕过白名单闸——
/// 修前 `(_, true) => {}` 对任意键吞空串；「空串=保持现值」只对白名单键有语义。
#[tokio::test(flavor = "multi_thread")]
async fn put_with_unknown_key_and_blank_value_is_422_not_noop() {
    let fx = fixture(None);
    let original = std::fs::read_to_string(&fx.config_path).unwrap();
    let (status, body) = oneshot(
        &fx.app,
        "PUT",
        "/api/config",
        Some(json!({"evil": {"new_key": ""}})),
    )
    .await;
    assert_eq!(status, 422, "错键+空串不得吞: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("evil.new_key"),
        "{body}"
    );
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
    std::fs::write(
        &config_path,
        common::yaml_template(
            Some("# 本行是用户的注释——PUT 后不得丢。"),
            "m5b-app",
            "SESSDATA=supersecret",
            "supersecret-key",
            "http://127.0.0.1:9/v1",
            "m5b-app",
        )
        .replace("OUTPUT_DIR", "./out"),
    )
    .unwrap();
    let app = build_app(AppState {
        config_path,
        web_root: dist,
        registry: live_server::registry::Registry::new(),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
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
async fn overview_without_collection_is_404() {
    let fx = fixture(None);
    // 未运行 → 404（空态明确）。POST /api/runs 的 B3b 钉见 tests/app_runs.rs。
    let (status, _) = oneshot(&fx.app, "GET", "/api/rooms/983/overview", None).await;
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------------
// 修复批钉团（8-agent 盲评裁定）
// ---------------------------------------------------------------------------

/// 非字符串值（数字/布尔/null）一律 422——曾被
/// `as_str().unwrap_or_default()` 沉默成「空串 = 保持」。
#[tokio::test(flavor = "multi_thread")]
async fn config_put_rejects_non_string_values_422_and_untouched() {
    let fx = fixture(None);
    let before = std::fs::read_to_string(&fx.config_path).unwrap();
    for body in [
        json!({"ai": {"model": 42}}),
        json!({"ai": {"model": true}}),
        json!({"bilibili": {"cookie": null}}),
        json!({"ai": {"base_url": {"nested": 1}}}),
    ] {
        let (status, reply) = oneshot(&fx.app, "PUT", "/api/config", Some(body.clone())).await;
        assert_eq!(status, 422, "case {body} → {reply}");
        let error = reply["error"].as_str().expect("error 文案");
        assert!(error.contains("值必须是字符串"), "case {body} → {error}");
        assert_eq!(
            std::fs::read_to_string(&fx.config_path).expect("config"),
            before,
            "拒绝后原文件不得有变"
        );
    }
}

/// 多行值拒绝——行级重写只承载单行 scalar（块标量会撕裂布局），且原文件不动。
#[tokio::test(flavor = "multi_thread")]
async fn config_put_rejects_multiline_values_422_and_untouched() {
    let fx = fixture(None);
    let before = std::fs::read_to_string(&fx.config_path).unwrap();
    let (status, reply) = oneshot(
        &fx.app,
        "PUT",
        "/api/config",
        Some(json!({"bilibili": {"cookie": "line1\nline2"}})),
    )
    .await;
    assert_eq!(status, 422, "{reply}");
    let error = reply["error"].as_str().expect("error 文案");
    assert!(error.contains("多行"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&fx.config_path).expect("config"),
        before,
        "拒绝后原文件分毫未动"
    );
}

/// 原子写成功路径不留 tmp 残渣，失败路径同样清洁。
#[tokio::test(flavor = "multi_thread")]
async fn config_put_atomic_write_leaves_no_tmp_residue() {
    let fx = fixture(None);
    let (status, reply) = oneshot(
        &fx.app,
        "PUT",
        "/api/config",
        Some(json!({"ai": {"model": "next-model"}})),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    let then_fail = oneshot(
        &fx.app,
        "PUT",
        "/api/config",
        Some(json!({"bilibili": {"cookie": "bad\nvalue"}})),
    )
    .await;
    assert_eq!(then_fail.0, 422, "{}", then_fail.1);
    let residue: Vec<_> = std::fs::read_dir(fx._tmp.path())
        .expect("tmp dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains("live-server-tmp"))
        .collect();
    assert!(residue.is_empty(), "tmp 残渣：{residue:?}");
}

/// 魔数与状态机字面的持久钉——盲评裁定「命名魔数必须有测试」，
/// 本钉把数值/字面锁到现形；有意调整需先改钉并说明。
#[test]
fn named_security_knobs_and_state_machine_litera_pin() {
    use live_server::app::{
        DEFAULT_PORT, MAX_PUT_VALUE_CHARS, MAX_REQUEST_BODY_BYTES, MAX_VID_PATH_CHARS,
        MAX_VIEWER_UID_CHARS, WRITABLE_CONFIG_KEYS,
    };
    assert_eq!(DEFAULT_PORT, 3781);
    assert_eq!(MAX_REQUEST_BODY_BYTES, 64 * 1024);
    assert_eq!(MAX_PUT_VALUE_CHARS, 4096);
    assert_eq!(MAX_VIEWER_UID_CHARS, 32);
    assert_eq!(MAX_VID_PATH_CHARS, 64);
    assert_eq!(
        WRITABLE_CONFIG_KEYS,
        [
            ("bilibili", "cookie"),
            ("ai", "api_key"),
            ("ai", "base_url"),
            ("ai", "model"),
            ("ai", "run_budget_cny")
        ]
    );
    assert_eq!(
        live_server::registry::RUN_STATES,
        [
            "queued",
            "collecting",
            "recording",
            "episodes",
            "per_viewer_ai",
            "audience",
            "done",
            "failed"
        ]
    );
    assert_eq!(live_server::registry::RUN_KINDS, ["full", "viewer"]);
}

// ---------------------------------------------------------------------------
// GET /api/budget —— 薄预估面（删码刀3 收口：月耗账本随 history.jsonl 同删，
// 实耗真相唯一源 = ai/state.json usage；月对账走平台计费后台）。
// ---------------------------------------------------------------------------

/// 钉（a）：budge_cny 无预算行 → null；写入预算行 → 回读 Some。
#[tokio::test(flavor = "multi_thread")]
async fn budget_get_exposes_null_then_some_budget_cny() {
    let fx = fixture(None);
    let (status, body) = oneshot(&fx.app, "GET", "/api/budget", None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["budget_cny"].is_null(), "{body}");
    // 写入预算行后回读 Some。
    let yaml = std::fs::read_to_string(&fx.config_path).unwrap();
    std::fs::write(
        &fx.config_path,
        yaml.replace(
            "  api: chat_completions",
            "  api: chat_completions\n  run_budget_cny: \"2.00\"",
        ),
    )
    .unwrap();
    let (status, body) = oneshot(&fx.app, "GET", "/api/budget", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["budget_cny"], 2.0, "{body}");
}

/// 钉（b）：名册/baseline 缺席（空 output 根）→ estimate 四字段全 null
/// （前端「预估 —」不臆造）。
#[tokio::test(flavor = "multi_thread")]
async fn budget_get_estimate_all_null_when_roster_missing() {
    let fx = fixture(None);
    let (status, body) = oneshot(&fx.app, "GET", "/api/budget", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["estimate"],
        json!({"roster_viewers": null, "fresh_viewers": null, "estimated_cny": null, "etd_minutes": null}),
        "{body}"
    );
}

/// 钉（c）：demo 布景（3 观众、零缓存）→ roster=3、fresh=3（无完整旧结论
/// 即新鲜）、estimated=3×1.5+1.5=6.0（闸同公式）、etd 上取整 [4,6]。
/// fresh 口径 = budget 闸同源（pipeline::roster_estimate）。
#[tokio::test(flavor = "multi_thread")]
async fn budget_get_estimate_pins_demo_roster_all_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "budget-ctl",
            "SESSDATA=test",
            "test-key",
            "http://127.0.0.1:9/v1",
            "budget-ctl",
        )
        .replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    let config = live_core::config::load_config(&config_path).expect("config loads");
    live_core::demo::build_demo(&config, Some(&output_dir)).expect("demo builds");
    let app = build_app(AppState {
        config_path,
        web_root: tmp.path().join("no-dist"),
        registry: live_server::registry::Registry::new(),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });
    let (status, body) = oneshot(&app, "GET", "/api/budget", None).await;
    assert_eq!(status, 200, "{body}");
    let estimate = &body["estimate"];
    assert_eq!(estimate["roster_viewers"], 3, "{estimate}");
    assert_eq!(
        estimate["fresh_viewers"], 3,
        "零缓存全员新鲜（无完整旧结论即新鲜）：{estimate}"
    );
    assert!(
        (estimate["estimated_cny"].as_f64().unwrap() - 6.0).abs() < 1e-9,
        "3 fresh × ¥1.5 + ¥1.5 audience = ¥6.0：{estimate}"
    );
    let etd = estimate["etd_minutes"].as_array().unwrap();
    // 常量带宽：3×(40..90)s + 90s 底 → 210s..360s → 上取整 4..6 分钟。
    assert_eq!(etd[0], 4, "{estimate}");
    assert_eq!(etd[1], 6, "{estimate}");
}
