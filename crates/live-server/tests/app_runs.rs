//! M5-B3b runs 通道钉团：POST 校验 + spawn 生命周期（G3）。
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

mod common;

use live_server::app::{AppState, MAX_REQUEST_BODY_BYTES, MAX_VIEWER_UID_CHARS, build_app};
use live_server::registry::{RUN_RECORDS_CAP, Registry, RunRecord};

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    registry: Registry,
}

fn fixture(bilibili_hosts: Option<(String, String)>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "m5b-runs",
            "SESSDATA=test",
            "test-key",
            "http://127.0.0.1:9/v1",
            "m5b-runs",
        )
        .replace(
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
        bilibili_hosts,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
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
// POST /api/runs 校验电池
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn runs_post_validation_battery_422() {
    let fx = fixture(None);
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
        // 分层四 kind——不接 uid、不接 force（每种矩阵臂各抽查一个全面代表，
        // 新 kinds 自身合法性由 202 侧钉跑道另行兜底）。
        json!({"kind": "collect_streamer", "viewer_uid": "123"}),
        json!({"kind": "collect_guards", "force": true}),
        json!({"kind": "ai_viewers", "force": true}),
        json!({"kind": "ai_audience", "viewer_uid": "123"}),
        // spend_mode 只收字面 incremental / briefing_only——分号以外的
        // 任何值（含 spend_mode=normal、中文、非字符串）都 422。
        json!({"kind": "full", "spend_mode": "normal"}),
        json!({"kind": "full", "spend_mode": "分层"}),
        json!({"kind": "full", "spend_mode": 1}),
        // spend_mode 只对带单人感知段的 kind（full/viewer/ai_viewers）合法，
        // 四分层无单人感知 → 携该键一律 422。
        json!({"kind": "collect_streamer", "spend_mode": "incremental"}),
        json!({"kind": "collect_guards", "spend_mode": "briefing_only"}),
        json!({"kind": "ai_audience", "spend_mode": "incremental"}),
    ];
    for case in &cases {
        let (status, body) = oneshot(&fx.app, "POST", "/api/runs", Some(case.clone())).await;
        assert_eq!(status, 422, "case {case} → {body}");
        assert!(
            body["error"].as_str().is_some_and(|s| !s.is_empty()),
            "{body}"
        );
    }
    // 对账：副作用断言必须是「零登记」对账，不是选一个不存在的键自嗨。
    assert_eq!(fx.registry.record_count(), 0, "校验电池不得登记任何 run");
}

/// spend_mode 双钉：非字面 → 422 且错文指向白名单；合法字面落在
/// 无单人感知段 kind 上 → 422 且错文指向 kind 语义。两钉合起来证明 parse
/// 复用 + kind 语义门都在生效。
#[tokio::test(flavor = "multi_thread")]
async fn runs_post_spend_mode_bad_literal_and_kind_mismatch_both_422() {
    let fx = fixture(None);
    for (case, needle) in [
        (
            json!({"kind": "full", "spend_mode": "normal"}),
            "incremental / briefing_only",
        ),
        (
            json!({"kind": "full", "spend_mode": "分层"}),
            "incremental / briefing_only",
        ),
        (
            json!({"kind": "full", "spend_mode": true}),
            "incremental / briefing_only",
        ),
        (
            json!({"kind": "collect_guards", "spend_mode": "briefing_only"}),
            "无单人感知段",
        ),
        (
            json!({"kind": "ai_audience", "spend_mode": "incremental"}),
            "无单人感知段",
        ),
    ] {
        let (status, body) = oneshot(&fx.app, "POST", "/api/runs", Some(case.clone())).await;
        assert_eq!(status, 422, "case {case} → {body}");
        let error = body["error"].as_str().expect("error 文案");
        assert!(
            error.contains(needle),
            "case {case} 错文须含 {needle}: {error}"
        );
    }
    assert_eq!(
        fx.registry.record_count(),
        0,
        "spend_mode 422 不得登记任何 run"
    );
}
// 分层四 kind 的「合法体面」（202/409 区别、行为面）由 app_runs_e2e 三钉兜底——
// 本文件的 fixture 指向真实 B 站端点（SESSDATA=test 布景），合法分面会直接产真实网络，
// 不在这里钉。

/// viewer_uid 穿透必须短路在 422 且绝不落盘——
/// 字符集白名单 [A-Za-z0-9_-]，dot-dot/内嵌斜杠/非 ASCII 一律拒（r6 「不落盘」是证据主位）。
#[tokio::test(flavor = "multi_thread")]
async fn runs_post_rejects_malicious_viewer_uid_without_side_effects() {
    let fx = fixture(None);
    for uid in ["..", "../escape", "12/34", "USP-中"] {
        let (status, body) = oneshot(
            &fx.app,
            "POST",
            "/api/runs",
            Some(json!({"kind": "viewer", "viewer_uid": uid})),
        )
        .await;
        assert_eq!(status, 422, "uid={uid} → {body}");
        assert!(
            body["error"].as_str().unwrap_or_default().contains("非法"),
            "错文须指向字符集守卫：{body}"
        );
    }
    assert_eq!(fx.registry.record_count(), 0, "穿透拒不得登记 run");
    assert!(
        !fx._tmp.path().join("out").join("viewers").exists(),
        "穿透拒不得落盘任何观众文件"
    );
}

// ---------------------------------------------------------------------------
// spawn 生命周期：collect 渠道真实启动 → wiremock 404 → failed（确定性、无外呼）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn spawn_run_full_lifecycle_collect_failure_marks_failed() {
    let mock = wiremock::MockServer::start().await;
    let fx = fixture(Some((mock.uri(), mock.uri())));

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

    // GET /api/runs/{id} 回放同一终态（轮询载体同形）。
    let (status, via_http) = oneshot(&fx.app, "GET", &format!("/api/runs/{run_id}"), None).await;
    assert_eq!(status, 200, "{via_http}");
    assert_eq!(via_http["status"], "failed");
    assert_eq!(via_http["kind"], "full");
    assert_eq!(via_http["viewer_uid"], Value::Null);
}

// ---------------------------------------------------------------------------
// 修复批钉团（8-agent 盲评裁定）
// ---------------------------------------------------------------------------

/// 已有未终态 run → 409，错文携带在飞 run_id。
#[tokio::test(flavor = "multi_thread")]
async fn runs_post_rejects_second_active_run_409() {
    let fx = fixture(None);
    fx.registry.register(RunRecord {
        run_id: "run-in-flight".to_string(),
        kind: "full".to_string(),
        viewer_uid: None,
        force: false,
        status: "collecting".to_string(),
        started_at: "t0".to_string(),
        finished_at: None,
        outcome: None,
        partial: false,
        events: Arc::new(live_core::events::RunEvents::new()),
    });
    let (status, body) = oneshot(&fx.app, "POST", "/api/runs", Some(json!({"kind": "full"}))).await;
    assert_eq!(status, 409, "{body}");
    let error = body["error"].as_str().expect("error 文案");
    assert!(error.contains("run-in-flight"), "{error}");
    // 未登记第二个 run：在飞的仍是唯一记录。
    let (status, snapshot) = oneshot(&fx.app, "GET", "/api/runs/run-in-flight", None).await;
    assert_eq!(status, 200, "{snapshot}");
    assert_eq!(snapshot["status"], "collecting");
}

/// 超上限 body → axum 原生 413 纯文本被信封化为 JSON {error}。
#[tokio::test(flavor = "multi_thread")]
async fn runs_post_oversized_body_is_413_json_envelope() {
    let fx = fixture(None);
    let payload = format!(
        r#"{{"kind":"full","pad":"{}"}}"#,
        "x".repeat(MAX_REQUEST_BODY_BYTES)
    );
    let request = Request::builder()
        .method("POST")
        .uri("/api/runs")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(payload))
        .unwrap();
    let response = fx.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status().as_u16(), 413);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.contains("application/json"),
        "413 必须是 JSON 信封：{content_type}"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("413 体必须可解析为 JSON");
    assert!(body["error"].is_string(), "{body}");
}

/// partial 契约键 viewer_stage_status 的单元钉——
/// 双向复现老死键之错：老读法 `status=="viewer_complete"` 在真实失败姿势下假阴性，
/// 在幻觉姿势下假阳性。
#[test]
fn viewer_stage_complete_reads_contract_key() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state.json");
    assert!(
        !live_server::registry::viewer_stage_complete(&state),
        "文件缺失 → false"
    );
    std::fs::write(&state, "not json").unwrap();
    assert!(
        !live_server::registry::viewer_stage_complete(&state),
        "非法 JSON → false"
    );
    std::fs::write(
        &state,
        r#"{"status":"failed","viewer_stage_status":"complete"}"#,
    )
    .unwrap();
    assert!(
        live_server::registry::viewer_stage_complete(&state),
        "audience 期失败姿势 → true（老读法假阴性之复现）"
    );
    std::fs::write(
        &state,
        r#"{"status":"viewer_complete","viewer_stage_status":"incomplete"}"#,
    )
    .unwrap();
    assert!(
        !live_server::registry::viewer_stage_complete(&state),
        "老死键幻觉姿势 → false（老读法假阳性之复现）"
    );
}

/// 时间闸钉：viewer_stage_status=complete 但 updated_at 早于本轮
/// started_at 的是旧轮次底票——不算本轮数据面；缺 updated_at 同按旧票处理（保守）。
#[test]
fn viewer_stage_complete_since_rejects_stale_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state.json");
    std::fs::write(
        &state,
        r#"{"viewer_stage_status":"complete","updated_at":"2026-08-01T00:00:00+00:00"}"#,
    )
    .unwrap();
    assert!(
        !live_server::registry::viewer_stage_complete_since(&state, "2026-08-01T00:00:01+00:00"),
        "updated_at 早于 started_at → 旧票拒"
    );
    assert!(
        live_server::registry::viewer_stage_complete_since(&state, "2026-07-31T23:59:59+00:00"),
        "updated_at 晚于 started_at → 本轮新面"
    );
    std::fs::write(&state, r#"{"viewer_stage_status":"complete"}"#).unwrap();
    assert!(
        !live_server::registry::viewer_stage_complete_since(&state, "2026-07-31T23:59:59+00:00"),
        "缺 updated_at 按旧票处理"
    );
}

/// 登记簿 GC——终态记录从旧到新剔除至 RUN_RECORDS_CAP；在飞记录永不剔。
#[test]
fn registry_gc_evicts_oldest_terminal_records_to_cap() {
    let registry = Registry::new();
    let seed = |suffix: &str, status: &str, started_at: &str| RunRecord {
        run_id: format!("run-gc-{suffix}"),
        kind: "full".to_string(),
        viewer_uid: None,
        force: false,
        status: status.to_string(),
        started_at: started_at.to_string(),
        finished_at: None,
        outcome: None,
        partial: false,
        events: Arc::new(live_core::events::RunEvents::new()),
    };
    registry.register(seed(
        "old-in-flight",
        "collecting",
        "2026-01-01T00:00:00+00:00",
    ));
    let last = RUN_RECORDS_CAP + 2;
    for index in 0..=last {
        registry.register(seed(
            &format!("{index:02}"),
            "done",
            &format!("2026-02-01T01:{:02}:{:02}+00:00", index / 60, index % 60),
        ));
    }
    assert_eq!(registry.record_count(), RUN_RECORDS_CAP);
    assert!(
        registry.get("run-gc-old-in-flight").is_some(),
        "在飞记录永不被剔除"
    );
    assert!(registry.get("run-gc-00").is_none(), "最旧终态记录被剔除");
    assert!(
        registry.get(&format!("run-gc-{last:02}")).is_some(),
        "最新登记必须在场"
    );
}

/// collect 期失败时 partial 必须为 false（集成姿势语义钉）。
/// 重采（reset）后旧的 ai/state.json 不再被推倒——文件原地留存，
/// partial=false 由时间闸兜底：栽的 state 无 updated_at → 按旧票拒收，
/// 不传「旧轮次」进本轮数据面。同钉顺手钉住保护面本身：终态后 state.json 必须还在。
#[tokio::test(flavor = "multi_thread")]
async fn collect_failure_keeps_prior_ai_state_and_reports_partial_false() {
    let mock = wiremock::MockServer::start().await;
    let fx = fixture(Some((mock.uri(), mock.uri())));
    let state_dir = fx._tmp.path().join("out").join("ai");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(
        state_dir.join("state.json"),
        r#"{"status":"failed","viewer_stage_status":"complete"}"#,
    )
    .unwrap();

    let (status, body) = oneshot(&fx.app, "POST", "/api/runs", Some(json!({"kind": "full"}))).await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().expect("run_id 字符串").to_string();

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
    assert_eq!(
        terminal["partial"],
        Value::Bool(false),
        "collect 期失败：旧 state 原地留存但无 updated_at 旧票拒收 → partial=false：{terminal}"
    );
    // 保护钉：重采（含失败路径）不得推倒认知层——旧 state.json 原地留存。
    assert!(
        state_dir.join("state.json").exists(),
        "Z5 重采保 AI：collect 失败路径同样不得碾平 ai/state.json"
    );
}
