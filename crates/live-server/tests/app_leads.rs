//! G2-B（工作项 1）leads 审批缝钉团的 G2 表形态（design §9.2 行 254）：
//! POST /api/rooms/:uid/leads/:lead_id/approve —— 账面 = discovery_leads 表。
//! 四钉——正常翻转 / 幂等重放（终态同、表行不动）/ 404（不存在）/
//! 422（非法迁移，错文讲规则）。
//!
//! 布景 = yaml_template + 手工置账（无需 demo 构建面；端点只碰图库表）。

use serde_json::Value;
use tower::ServiceExt;

mod common;

use axum::http::Request;
use http_body_util::BodyExt;

use live_core::graph::store::Store;
use live_core::leads::{self, LeadStatus, LedgerRow};
use live_server::app::{AppState, build_app};

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    data_root: std::path::PathBuf,
}

fn row(key: &str, lead_type: &str, status: LeadStatus) -> LedgerRow {
    LedgerRow {
        dedupe_key: key.into(),
        lead_type: lead_type.into(),
        locator: format!("loc-{key}"),
        motivation: "m".into(),
        expected_signal: "s".into(),
        priority: "high".into(),
        evidence_ids: vec![],
        viewer_id: "u".into(),
        first_seen_run_id: "run:a".into(),
        created_at: "t".into(),
        status,
        yield_count: 0,
        resolution_note: String::new(),
        reject_chips: Vec::new(),
        reject_note: String::new(),
    }
}

fn ledger_rows(root: &std::path::Path) -> Vec<LedgerRow> {
    let store = Store::open(&root.join("graph/perception.sqlite3")).expect("store opens");
    leads::read_rows(&store).expect("rows read")
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
            "g2b-leads",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "g2b-leads",
        )
        .replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    // 置账：四态各一行（非法迁移钉的源态面）。first_seen_run_id 外键锚
    // 必须先有 graph_run。
    let store = Store::open(&output_dir.join("graph/perception.sqlite3")).unwrap();
    store.begin_run_fixed("run:a", "t0", "m").unwrap();
    let rows = [
        row("k-pending", "search", LeadStatus::PendingApproval),
        row("k-approved", "video", LeadStatus::Approved),
        row("k-consumed", "creator", LeadStatus::Consumed),
        row("k-rejected", "search", LeadStatus::Rejected),
    ];
    let refs: Vec<&LedgerRow> = rows.iter().collect();
    store.insert_lead_rows(&refs, true).unwrap();
    drop(store);
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

async fn post(app: &axum::Router, path: &str) -> (u16, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// 带体 POST（reject 端点用）——体缺省无 body 的语义由空体测试单独钉。
async fn post_json(app: &axum::Router, path: &str, body: &str) -> (u16, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// 钉①正常翻转：pending → approved 落表；其余状态行逐字段原样。
#[tokio::test(flavor = "multi_thread")]
async fn approve_flips_pending_row_to_approved() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    let (status, body) = post(&fx.app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dedupe_key"], "k-pending");
    assert_eq!(body["status"], "approved", "{body}");
    assert_eq!(body["changed"], true, "{body}");
    let after = ledger_rows(&fx.data_root);
    assert_eq!(after.len(), 4, "行数不变");
    let flipped = after.iter().find(|r| r.dedupe_key == "k-pending").unwrap();
    assert_eq!(flipped.status, LeadStatus::Approved);
    // 其余行逐字段原样（dedupe_key 各行唯一，剔目标行后逐行全等）。
    let strip = |rows: &[LedgerRow]| {
        rows.iter()
            .filter(|r| r.dedupe_key != "k-pending")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&before), strip(&after), "其余行逐字段原样：{after:?}");
    // 翻转只有状态一个字段变（留痕位不发明）
    let before_target = before.iter().find(|r| r.dedupe_key == "k-pending").unwrap();
    let mut expected = before_target.clone();
    expected.status = LeadStatus::Approved;
    assert_eq!(*flipped, expected);
}

/// 钉②幂等重放：重复调返回相同终态（status=approved、changed=false），
/// 表行逐字段不动。
#[tokio::test(flavor = "multi_thread")]
async fn approve_replay_returns_same_terminal_state_without_rewrite() {
    let fx = fixture();
    // 已 approved 行直调
    let before = ledger_rows(&fx.data_root);
    let (status, body) = post(&fx.app, "/api/rooms/983/leads/k-approved/approve").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "approved", "{body}");
    assert_eq!(body["changed"], false, "{body}");
    assert_eq!(
        ledger_rows(&fx.data_root),
        before,
        "幂等重放不得写表（行面逐字段不动）"
    );
    // 翻转后再重放：同一终态
    let (status, first) = post(&fx.app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["changed"], true);
    let settled = ledger_rows(&fx.data_root);
    let (status, second) = post(&fx.app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(second["dedupe_key"], first["dedupe_key"]);
    assert_eq!(second["status"], first["status"], "终态必须相同");
    assert_eq!(second["changed"], false, "重放无动作");
    assert_eq!(ledger_rows(&fx.data_root), settled, "重放后表行不变");
}

/// 钉③不存在 = 404：未知 lead_id / 错房间 / 穿透形 lead_id 三臂，表均不动。
#[tokio::test(flavor = "multi_thread")]
async fn approve_unknown_lead_or_room_is_404() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    for path in [
        "/api/rooms/983/leads/不存在/approve",
        "/api/rooms/983/leads/no-such-key/approve",
        "/api/rooms/983/leads/..%2F..%2Fevidence-sink/approve",
        "/api/rooms/983/leads/with.dot/approve",
    ] {
        let (status, body) = post(&fx.app, path).await;
        assert_eq!(status, 404, "{path} → {body}");
        assert!(
            body["error"].as_str().is_some_and(|s| s.contains("不存在")),
            "{path} → {body}"
        );
    }
    // 错房间：uid 999 不在 config → room_guard 拒（错误形态同族）。
    let (status, _body) = post(&fx.app, "/api/rooms/999/leads/k-pending/approve").await;
    assert_eq!(status, 404, "错房间 → 404");
    assert_eq!(ledger_rows(&fx.data_root), before, "404 面不得碰表");
}

/// 钉④非法迁移 = 422：consumed/rejected 源态皆拒（状态机单行道的
/// 唯一源态是 pending_approval），错文讲规则 + 当前状态，表行逐字段不动。
#[tokio::test(flavor = "multi_thread")]
async fn approve_rejects_illegal_transitions_422() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    for (lead_id, current) in [("k-consumed", "consumed"), ("k-rejected", "rejected")] {
        let (status, body) =
            post(&fx.app, &format!("/api/rooms/983/leads/{lead_id}/approve")).await;
        assert_eq!(status, 422, "{lead_id} → {body}");
        let error = body["error"].as_str().expect("err text");
        assert!(
            error.contains("pending_approval") && error.contains("approved"),
            "错文必须讲单行规则：{error}"
        );
        assert!(
            error.contains(current),
            "错文必须点出当前源态 {current}：{error}"
        );
    }
    assert_eq!(ledger_rows(&fx.data_root), before, "422 面不得碰表");
}

// ---------------------------------------------------------------------------
// leads 拒绝缝：POST /api/rooms/:uid/leads/:lead_id/reject（单 reason 形态）
// ---------------------------------------------------------------------------

/// 钉①正常翻转：pending → rejected；空体（空拒因）→ 留档列落 NULL。
#[tokio::test(flavor = "multi_thread")]
async fn reject_flips_pending_row_to_rejected_with_empty_reason() {
    let fx = fixture();
    let (status, body) = post(&fx.app, "/api/rooms/983/leads/k-pending/reject").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["changed"], true, "{body}");
    assert_eq!(body["status"], "rejected", "{body}");
    assert_eq!(body["reject_note"], "", "{body}");
    let flipped = ledger_rows(&fx.data_root)
        .into_iter()
        .find(|r| r.dedupe_key == "k-pending")
        .unwrap();
    assert_eq!(flipped.status, LeadStatus::Rejected);
    assert!(flipped.reject_note.is_empty(), "空拒因留档为空");
}

/// 钉②拒因随体落库（trim 规范化后落账）；已 rejected 重放幂等：
/// 空体/同参/异参一律 200 相同终态、表行不动（新 reason 不覆盖留档）。
#[tokio::test(flavor = "multi_thread")]
async fn reject_records_reason_and_replay_never_overwrites() {
    let fx = fixture();
    let (status, body) = post_json(
        &fx.app,
        "/api/rooms/983/leads/k-pending/reject",
        r#"{"reason":" 主播不玩这品类  "}"#,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["changed"], true, "{body}");
    assert_eq!(body["reject_note"], "主播不玩这品类", "{body}");
    let settled = ledger_rows(&fx.data_root);
    let flipped = settled
        .iter()
        .find(|r| r.dedupe_key == "k-pending")
        .unwrap()
        .clone();
    assert_eq!(flipped.reject_note, "主播不玩这品类", "端点 trim 后落账");
    // 同参重放：幂等
    let (status, replay) = post_json(
        &fx.app,
        "/api/rooms/983/leads/k-pending/reject",
        r#"{"reason":"主播不玩这品类"}"#,
    )
    .await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["changed"], false, "{replay}");
    assert_eq!(ledger_rows(&fx.data_root), settled, "同参重放不写表");
    // 异参（新 reason）→ 200 幂等，留档不被改写（改判先谈人，不靠端点打架）
    let (status, rewrite) = post_json(
        &fx.app,
        "/api/rooms/983/leads/k-pending/reject",
        r#"{"reason":"换一套说辞"}"#,
    )
    .await;
    assert_eq!(status, 200, "{rewrite}");
    assert_eq!(rewrite["changed"], false, "{rewrite}");
    assert_eq!(
        ledger_rows(&fx.data_root),
        settled,
        "异参重放不写表（留档不可覆盖）"
    );
}

/// 钉③不存在 = 404：未知 lead_id / 错房间 / 穿透形 id，表均不动。
#[tokio::test(flavor = "multi_thread")]
async fn reject_unknown_lead_or_room_is_404() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    for path in [
        "/api/rooms/983/leads/不存在/reject",
        "/api/rooms/983/leads/no-such-key/reject",
        "/api/rooms/983/leads/..%2F..%2Fevidence-sink/reject",
    ] {
        let (status, body) = post(&fx.app, path).await;
        assert_eq!(status, 404, "{path} → {body}");
        assert!(
            body["error"].as_str().is_some_and(|s| s.contains("不存在")),
            "{path} → {body}"
        );
    }
    let (status, _body) = post(&fx.app, "/api/rooms/999/leads/k-pending/reject").await;
    assert_eq!(status, 404, "错房间 → 404");
    assert_eq!(ledger_rows(&fx.data_root), before, "404 面不得碰表");
}

/// 钉④非法迁移 = 422：consumed/approved 源态皆拒（拒绝单行道的唯一源态是
/// pending_approval），错文讲规则 + 当前状态，表行逐字段不动。
#[tokio::test(flavor = "multi_thread")]
async fn reject_rejects_illegal_source_states_422() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    for (lead_id, current) in [("k-consumed", "consumed"), ("k-approved", "approved")] {
        let (status, body) = post(&fx.app, &format!("/api/rooms/983/leads/{lead_id}/reject")).await;
        assert_eq!(status, 422, "{lead_id} → {body}");
        let error = body["error"].as_str().expect("err text");
        assert!(
            error.contains("pending_approval") && error.contains("rejected"),
            "错文必须讲单行规则：{error}"
        );
        assert!(
            error.contains(current),
            "错文必须点出当前源态 {current}：{error}"
        );
    }
    assert_eq!(ledger_rows(&fx.data_root), before, "422 面不得碰表");
}

/// 钉⑤拒因参数校验 = 422：reason 超 80 字 / 体坏 JSON / 体非对象 /
/// reason 非字符串——任一脱靶都不落账。
#[tokio::test(flavor = "multi_thread")]
async fn reject_validates_reason_params_422() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    let mut cases: Vec<(String, &str)> = vec![
        (r#"{不是 json"#.into(), "JSON"),
        (r#""错误形态""#.into(), "对象"),
        (r#"{"reason":42}"#.into(), "字符串"),
        // 超长 reason 用例单独构造（repeat 不能在字面量里用）。
        (format!(r#"{{"reason":"{}"}}"#, "长".repeat(81)), "最多"),
    ];
    for (body, needle) in cases.drain(..) {
        let (status, resp) =
            post_json(&fx.app, "/api/rooms/983/leads/k-pending/reject", &body).await;
        assert_eq!(status, 422, "{body} → {resp}");
        assert!(
            resp["error"].as_str().is_some_and(|s| s.contains(needle)),
            "错文必须含 {needle}：{resp}"
        );
    }
    assert_eq!(ledger_rows(&fx.data_root), before, "422 面不得碰表");
    // 空体打在已 rejected 行上 = 幂等放行（前 422 均未落账）
    let (status, ok) = post(&fx.app, "/api/rooms/983/leads/k-rejected/reject").await;
    assert_eq!(status, 200, "{ok}");
    assert_eq!(ok["changed"], false, "{ok}");
}
