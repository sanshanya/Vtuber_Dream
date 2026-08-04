//! G2-B（工作项 1）leads 审批缝钉团：POST /api/rooms/:uid/leads/:lead_id/approve。
//! 四钉——正常翻转 / 幂等重放（终态同、账本不写重行）/ 404（不存在）/
//! 422（非法迁移，错文讲规则）。
//!
//! 布景 = yaml_template + 手工置账（无需 demo 构建面；端点只碰 leads.jsonl）。

use serde_json::Value;
use tower::ServiceExt;

mod common;

use axum::http::Request;
use http_body_util::BodyExt;

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
    }
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
    // 置账：五态各一行 + 消费行（非法迁移钉的源态面）。
    let rows = [
        row("k-pending", "search", LeadStatus::PendingApproval),
        row("k-approved", "video", LeadStatus::Approved),
        row("k-consumed", "creator", LeadStatus::Consumed),
        row("k-rejected", "search", LeadStatus::Rejected),
        row("k-deferred", "room", LeadStatus::Deferred),
    ];
    let text = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(leads::ledger_path(&output_dir), &text).unwrap();
    let app = build_app(AppState {
        config_path,
        web_root: tmp.path().join("no-dist"),
        registry: live_server::registry::Registry::new(),
        demo: false,
        data_root: None,
        bilibili_hosts: None,
        config_write_lock: Default::default(),
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

/// 钉①正常翻转：pending → approved 落账本；其余状态行字节级原样。
#[tokio::test(flavor = "multi_thread")]
async fn approve_flips_pending_row_to_approved() {
    let fx = fixture();
    let before = std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap();
    let (status, body) = post(&fx.app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dedupe_key"], "k-pending");
    assert_eq!(body["status"], "approved", "{body}");
    assert_eq!(body["changed"], true, "{body}");
    let rows = leads::read_ledger(&leads::ledger_path(&fx.data_root));
    assert_eq!(rows.len(), 5, "行数不变");
    let flipped = rows.iter().find(|r| r.dedupe_key == "k-pending").unwrap();
    assert_eq!(flipped.status, LeadStatus::Approved);
    // 其余行原样：逐行对照翻转前文本（dedupe_key 各行唯一，剔目标行后逐字节一致）。
    let after = std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap();
    let strip = |text: &str| {
        text.lines()
            .filter(|line| !line.contains("\"k-pending\""))
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    assert_eq!(strip(&before), strip(&after), "其余行字节级原样：\n{after}");
}

/// 钉②幂等重放：重复调返回相同终态（status=approved、changed=false），
/// 账本字节级不动——不重写、不增行。
#[tokio::test(flavor = "multi_thread")]
async fn approve_replay_returns_same_terminal_state_without_rewrite() {
    let fx = fixture();
    // 已 approved 行直调
    let before = std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap();
    let (status, body) = post(&fx.app, "/api/rooms/983/leads/k-approved/approve").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "approved", "{body}");
    assert_eq!(body["changed"], false, "{body}");
    assert_eq!(
        std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap(),
        before,
        "幂等重放不得写账本（含不写重行、不动 mtime 级内容）"
    );
    // 翻转后再重放：同一终态
    let (status, first) = post(&fx.app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["changed"], true);
    let settled = std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap();
    let (status, second) = post(&fx.app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(second["dedupe_key"], first["dedupe_key"]);
    assert_eq!(second["status"], first["status"], "终态必须相同");
    assert_eq!(second["changed"], false, "重放无动作");
    assert_eq!(
        std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap(),
        settled,
        "重放后账本字节不变"
    );
}

/// 钉③不存在 = 404：未知 lead_id / 错房间 / 穿透形 lead_id 三臂，账本均不动。
#[tokio::test(flavor = "multi_thread")]
async fn approve_unknown_lead_or_room_is_404() {
    let fx = fixture();
    let before = std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap();
    for path in [
        "/api/rooms/983/leads/不存在/approve",
        "/api/rooms/983/leads/no-such-key/approve",
        "/api/rooms/999/leads/k-pending/approve",
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
    assert_eq!(
        std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap(),
        before,
        "404 面不得碰账本"
    );
}

/// 钉④非法迁移 = 422：consumed/rejected/deferred 源态皆拒（状态机单行道的
/// 唯一源态是 pending_approval），错文讲规则 + 当前状态，账本字节级不动。
#[tokio::test(flavor = "multi_thread")]
async fn approve_rejects_illegal_transitions_422() {
    let fx = fixture();
    let before = std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap();
    for (lead_id, current) in [
        ("k-consumed", "consumed"),
        ("k-rejected", "rejected"),
        ("k-deferred", "deferred"),
    ] {
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
    assert_eq!(
        std::fs::read_to_string(leads::ledger_path(&fx.data_root)).unwrap(),
        before,
        "422 面不得碰账本"
    );
}
