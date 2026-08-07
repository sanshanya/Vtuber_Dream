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
        demo: false,
        data_root: None,
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

/// 轮2-R1-A③ 钉：拼错键+空串不得静默绕过白名单闸——
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
        demo: false,
        data_root: None,
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

// ---------------------------------------------------------------------------
// R5-yellow：demo→HTML 一体化（site 挂 M5 之后，run --demo 的数据面↔页面骨架同端）
// ---------------------------------------------------------------------------

/// R5-yellow 钉 demo：run --demo 的数据面（data_root = build_demo 产物）与页面骨架
/// （web_root = dist）必须同在一个 app 实例、同一端口两面可达——demo 是「历史数据
/// 一体呈现」的完整形态，不是只有 API 的数据桩。不钉出什么：dist 路由改名 / 产物
/// 注入失效会在这里静默回归（页面 200 但内容不再是 index，D5 有先例）。
#[tokio::test(flavor = "multi_thread")]
async fn demo_serve_closes_page_skeleton_and_data_surface() {
    let tmp = tempfile::tempdir().unwrap();
    // 页面骨架：dist 形态照抄 existing_dist_serves_index——合成 index.html 含可识别标记串。
    let dist = tmp.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("index.html"), "<h1>demo-index-ok</h1>").unwrap();
    // demo 数据面：照 app_data 的 demo fixture 姿势拿 config → build_demo 到临时输出根。
    let output_dir = tmp.path().join("out");
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "demo-serve",
            "SESSDATA=supersecret",
            "supersecret-key",
            "http://127.0.0.1:9/v1",
            "demo-serve",
        )
        .replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    let config = live_core::config::load_config(&config_path).expect("config loads");
    let built = live_core::demo::build_demo(&config, None).expect("demo builds");
    let demo_root = std::path::PathBuf::from(
        built["output_dir"]
            .as_str()
            .expect("demo reports output_dir"),
    );
    let app = build_app(AppState {
        config_path,
        web_root: dist,
        registry: live_server::registry::Registry::new(),
        demo: true,
        data_root: Some(demo_root),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });

    // ① 页面骨架：GET / → 200 且 index 标记串在（dist 装配在 demo 模式同样生效）。
    let request = Request::builder()
        .uri("/")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let page = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(page.contains("demo-index-ok"), "{page}");

    // ② 数据面：demo API 数据端点 → 200 且 demo 数据标志在（demo-1 观众行）。
    let (status, body) = oneshot(&app, "GET", "/api/rooms/983/viewers", None).await;
    assert_eq!(status, 200, "{body}");
    let viewers = body.as_array().expect("viewer list");
    assert!(
        viewers.iter().any(|row| row["uid"] == "demo-1"),
        "demo 数据面必须与页面骨架同端可达：{body}"
    );
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
// X1 修复批钉团（8-agent 盲评裁定）
// ---------------------------------------------------------------------------

/// ag2-F2/X1-b：非字符串值（数字/布尔/null）一律 422——曾被
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

/// ag2-F3/X1-c：多行值拒绝——行级重写只承载单行 scalar（块标量会撕裂布局），且原文件不动。
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

/// ag2-F3/X1-c：原子写成功路径不留 tmp 残渣，失败路径同样清洁。
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

/// ag6-F1/X3：D9 魔数与状态机字面的持久钉——盲评裁定「命名魔数必须有测试」，
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
// Z6 件3：history.jsonl 实耗账 + GET /api/budget
// ---------------------------------------------------------------------------

/// ts 前 7 字符 = "YYYY-MM"（UTC 月界，代码里 `&now_iso()[..7]` 即此口径）。
/// 给出与当前 UTC 月相邻的上一月号，供「跨两月」钉造数据。
fn previous_month(now: &str) -> String {
    let year: i32 = now[0..4].parse().expect("year");
    let month: u32 = now[5..7].parse().expect("month");
    if month == 1 {
        format!("{}-12", year - 1)
    } else {
        format!("{}-{:02}", year, month - 1)
    }
}

/// Z6 件3 钉（a）：读到坏行跳过、只按 UTC 月界聚合 cost，last_run 取最后有效行。
#[tokio::test(flavor = "multi_thread")]
async fn budget_get_monthly_sum_tolerates_bad_lines_and_crosses_months() {
    let fx = fixture(None);
    let now = live_core::episodes::now_iso();
    let current = now[..7].to_string();
    let before = previous_month(&now);
    let ledger = fx.config_path.parent().unwrap().join("out/history.jsonl");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    // 三行跨两月 + 一行坏数据（顺序刻意混杂：坏行在中间，证明不影响后续行）。
    std::fs::write(
        &ledger,
        format!(
            "{c1}\n{bad}\n{c2}\n{p}\n{c3}\n",
            c1 = json!({"ts": format!("{current}-01T00:00:00+00:00"), "run_id": "r1", "kind": "full",
                        "spend_mode": "normal", "status": "done", "usage": {"llm_requests": 1, "input_tokens": 100, "output_tokens": 10, "total_tokens": 110}, "cost_cny": 0.28}),
            bad = "这一行不是 JSON，必须被容忍跳过",
            c2 = json!({"ts": format!("{current}-15T12:00:00+00:00"), "run_id": "r2", "kind": "viewer",
                        "spend_mode": "briefing_only", "status": "done", "usage": {"llm_requests": 1, "input_tokens": 200, "output_tokens": 20, "total_tokens": 220}, "cost_cny": 0.56}),
            p = json!({"ts": format!("{before}-28T23:59:59+00:00"), "run_id": "r3", "kind": "full",
                       "spend_mode": "incremental", "status": "done", "usage": {"llm_requests": 1, "input_tokens": 300, "output_tokens": 30, "total_tokens": 330}, "cost_cny": 0.84}),
            c3 = json!({"ts": format!("{current}-20T08:00:00+00:00"), "run_id": "r4", "kind": "ai_viewers",
                        "spend_mode": "incremental", "status": "done", "cost_cny": 0.3}),
        ),
    )
    .unwrap();
    let (status, body) = oneshot(&fx.app, "GET", "/api/budget", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["month"], current);
    // 本月三行 = 0.28 + 0.56 + 0.3，上月 0.84 不计；坏行不绊也不计。
    assert_eq!(body["month_cost_cny"], 1.14);
    assert_eq!(body["month_runs"], 3);
    let last = &body["last_run"];
    assert_eq!(last["run_id"], "r4", "{last}");
    assert_eq!(
        last["ts"].as_str().unwrap(),
        &format!("{current}-20T08:00:00+00:00")
    );
    assert_eq!(last["cost_cny"], 0.3);
    assert_eq!(last["status"], "done");
    assert_eq!(last["kind"], "ai_viewers");
    assert_eq!(last["spend_mode"], "incremental");
}

/// Z6 件3 钉（b）：无 budget 行 → null；有 budget 行 → Some（前端「未设预算」分支）。
#[tokio::test(flavor = "multi_thread")]
async fn budget_get_exposes_null_then_some_budget_cny() {
    let fx = fixture(None);
    let now = live_core::episodes::now_iso();
    let current = now[..7].to_string();
    let ledger = fx.config_path.parent().unwrap().join("out/history.jsonl");
    // 未造账本：文件缺失 → 全零 + last_run null（前端首屏空态）。
    let (status, body) = oneshot(&fx.app, "GET", "/api/budget", None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["budget_cny"].is_null(), "{body}");
    assert_eq!(body["month"], current);
    assert_eq!(body["month_cost_cny"], 0.0);
    assert_eq!(body["month_runs"], 0);
    assert!(body["last_run"].is_null(), "{body}");
    // 有账本也先 null（模板默认无 run_budget_cny 行）。
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(
        &ledger,
        format!(
            "{}\n",
            json!({"ts": format!("{current}-01T00:00:00+00:00"), "run_id": "rx", "kind": "full",
                   "spend_mode": "normal", "status": "done", "usage": {"llm_requests": 1, "input_tokens": 100, "output_tokens": 10, "total_tokens": 110}, "cost_cny": 0.28})
        ),
    )
    .unwrap();
    let (status, body) = oneshot(&fx.app, "GET", "/api/budget", None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["budget_cny"].is_null(), "模板无预算行 → null：{body}");
    assert_eq!(body["month_cost_cny"], 0.28);
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

/// Z6 件3 钉（c）：collect_* 无 ai/state.json → usage 四零 + cost 0——
/// 账本诚实记账的最朴素姿势（不臆造任何 AI 消耗）。
#[test]
fn append_history_line_for_collect_kind_records_zero_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap(); // 无 ai/state.json：collect_* 从不写认知层
    let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let ring = emitted.clone();
    live_server::registry::append_history_line(
        &out,
        "run-collect",
        "collect_guards",
        live_core::agent::budget::SpendMode::Normal,
        "done",
        &move |message| ring.lock().expect("ring").push(message.to_string()),
    );
    let line = std::fs::read_to_string(out.join("history.jsonl")).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["kind"], "collect_guards");
    assert_eq!(record["status"], "done");
    assert_eq!(
        record["usage"],
        json!({"llm_requests": 0, "input_tokens": 0, "output_tokens": 0, "total_tokens": 0}),
        "collect_* 账本须诚实记零：{record}"
    );
    assert_eq!(record["cost_cny"], 0.0, "{record}");
    assert!(
        emitted.lock().expect("ring").is_empty(),
        "正常记账不响铃：{:?}",
        emitted.lock().expect("ring")
    );
}

/// Z6 件3 钉（c-2）：有 ai/state.json 的 run 按 usage 记 cost_cny（成本公式复算），
/// 且 usage 只取契约五键（丢弃 tool_calls）。
#[test]
fn append_history_line_reads_state_usage_and_drops_tool_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let ai = out.join("ai");
    std::fs::create_dir_all(&ai).unwrap();
    std::fs::write(
        ai.join("state.json"),
        r#"{"status":"done","usage":{"llm_requests":3,"tool_calls":9,
           "input_tokens":1250,"output_tokens":500,"total_tokens":1750}}"#,
    )
    .unwrap();
    let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let ring = emitted.clone();
    live_server::registry::append_history_line(
        &out,
        "run-full",
        "full",
        live_core::agent::budget::SpendMode::IncrementalOnly,
        "done",
        &move |message| ring.lock().expect("ring").push(message.to_string()),
    );
    let line = std::fs::read_to_string(out.join("history.jsonl")).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        record["usage"],
        json!({"llm_requests": 3, "input_tokens": 1250, "output_tokens": 500, "total_tokens": 1750}),
        "usage 只取契约四键：{record}"
    );
    assert_eq!(
        record["usage"].get("tool_calls"),
        None,
        "tool_calls 不得入账本：{record}"
    );
    // cost_cny = (1250*2 + 500*8)/1e6 = (2500+4000)/1e6 = 0.0065。
    assert_eq!(record["cost_cny"], 0.0065, "{record}");
    assert_eq!(record["spend_mode"], "incremental", "{record}");
    assert!(
        emitted.lock().expect("ring").is_empty(),
        "正常记账不响铃：{:?}",
        emitted.lock().expect("ring")
    );
}

/// Z6 件3 记账失败面：history.jsonl 写失败只响铃不改终态。
/// append_history_line 用只读目录模拟写失败（OpenOptions append 必然 Err）。
#[test]
fn append_history_line_write_failure_only_rings() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    // 目录以同名文件占位 → OpenOptions append 打不开（路径存在但非目录）。
    std::fs::write(out.join("history.jsonl"), "I am a file, not a dir").unwrap();
    let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let ring = emitted.clone();
    // 在 history.jsonl 同路径旁放一个同名「目录」不可能；改为把 history.jsonl 做成
    // 目录名，OpenOptions 以 append=true 打目录必然 Err——等价模拟写失败。
    // （write 前先毁掉刚才的文件占位，recreate 成目录。）
    std::fs::remove_file(out.join("history.jsonl")).unwrap();
    std::fs::create_dir(out.join("history.jsonl")).unwrap();
    live_server::registry::append_history_line(
        &out,
        "run-x",
        "full",
        live_core::agent::budget::SpendMode::Normal,
        "failed",
        &move |message| ring.lock().expect("ring").push(message.to_string()),
    );
    let rung = emitted.lock().expect("ring");
    assert_eq!(rung.len(), 1, "写失败必须响铃：{rung:?}");
    assert!(rung[0].contains("[LEDGER]"), "响铃须带账本前缀：{rung:?}");
}
