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
    // 置账：五态各一行 + 消费行（非法迁移钉的源态面）。first_seen_run_id 外键锚
    // 必须先有 graph_run。
    let store = Store::open(&output_dir.join("graph/perception.sqlite3")).unwrap();
    store.begin_run_fixed("run:a", "t0", "m").unwrap();
    let rows = [
        row("k-pending", "search", LeadStatus::PendingApproval),
        row("k-approved", "video", LeadStatus::Approved),
        row("k-consumed", "creator", LeadStatus::Consumed),
        row("k-rejected", "search", LeadStatus::Rejected),
        row("k-deferred", "room", LeadStatus::Deferred),
    ];
    let refs: Vec<&LedgerRow> = rows.iter().collect();
    store.insert_lead_rows(&refs, true).unwrap();
    drop(store);
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
    assert_eq!(after.len(), 5, "行数不变");
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

/// 钉④非法迁移 = 422：consumed/rejected/deferred 源态皆拒（状态机单行道的
/// 唯一源态是 pending_approval），错文讲规则 + 当前状态，表行逐字段不动。
#[tokio::test(flavor = "multi_thread")]
async fn approve_rejects_illegal_transitions_422() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
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
    assert_eq!(ledger_rows(&fx.data_root), before, "422 面不得碰表");
}

/// G2 迁移触点钉：账本还在旧 JSONL 形态时审批缝自己完成入库归档——
/// jsonl 消失、.bak 在场、翻转落在表上。
#[tokio::test(flavor = "multi_thread")]
async fn approve_migrates_legacy_jsonl_then_flips_row() {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    std::fs::create_dir_all(&output_dir).unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "g2b-leads-legacy",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "g2b-leads-legacy",
        )
        .replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    // 旧形态：只有 leads.jsonl、无图库。first_seen_run_id 指向的 run 无法在 v7 表外键
    // 下复活（旧账本的真实 run 在旧库里）——此时审批缝建库行 = run:a 缺位 → FK 拒。
    // 所以本钉预建图库 + run（等价于「该房间跑过 pipeline」的最小事实现场），再放账。
    let store = Store::open(&output_dir.join("graph/perception.sqlite3")).unwrap();
    store.begin_run_fixed("run:a", "t0", "m").unwrap();
    drop(store);
    let text = format!(
        "{}\n",
        serde_json::to_string(&row("k-pending", "search", LeadStatus::PendingApproval)).unwrap()
    );
    std::fs::write(leads::ledger_path(&output_dir), &text).unwrap();
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
    let (status, body) = post(&app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "approved", "{body}");
    // 迁移确证：jsonl 已经归档为 .bak、表里行已 flipped
    assert!(
        !leads::ledger_path(&output_dir).exists(),
        "审批缝须完成一次性迁移归档"
    );
    assert!(
        output_dir.join("leads.jsonl.bak").exists(),
        "归档 .bak 必须在场"
    );
    let rows = ledger_rows(&output_dir);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, LeadStatus::Approved);
    assert_eq!(rows[0].dedupe_key, "k-pending");
    assert_eq!(rows[0].lead_type, "search");
}

/// 补钉：approve 缝的端点响铃——legacy leads.jsonl 含坏行时
/// 必须 500（守卫停火）、文件原地、归档缺席、表不被半份导入。
#[tokio::test(flavor = "multi_thread")]
async fn approve_with_bad_legacy_jsonl_rings_500_and_stays_put() {
    let fx = fixture();
    std::fs::write(
        leads::ledger_path(&fx.data_root),
        "{\"dedupe_key\":\"k-bad\"}\n{不是合法 json\n",
    )
    .unwrap();
    // 表起初有自己的合法行也不许被坏账本扰动：借 k-pending 走 approve → 500。
    let (status, body) = post(&fx.app, "/api/rooms/983/leads/k-pending/approve").await;
    assert_eq!(status, 500, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|s| s.contains("迁移守卫停火")),
        "{body}"
    );
    // 守卫的三不倒：文件原地、.bak 缺席、表行零翻动（k-pending 仍未批）。
    assert!(leads::ledger_path(&fx.data_root).exists(), "坏账本须原地");
    assert!(
        !fx.data_root.join("leads.jsonl.bak").exists(),
        "守卫停火绝不归档"
    );
    assert!(
        ledger_rows(&fx.data_root)
            .iter()
            .all(|row| row.status == LeadStatus::PendingApproval
                || row.status != LeadStatus::Approved
                || row.dedupe_key != "k-pending"),
        "表行零翻动"
    );
}

// ---------------------------------------------------------------------------
// leads 拒绝缝：POST /api/rooms/:uid/leads/:lead_id/reject
// ---------------------------------------------------------------------------

/// 钉①正常翻转：pending → rejected；空体（全空拒因）→ 留档列落 NULL/NULL，
/// 其余状态行逐字段原样；翻转只有「状态 + 拒因两列」变（留痕位不发明）。
#[tokio::test(flavor = "multi_thread")]
async fn reject_flips_pending_row_to_rejected_with_empty_reasons() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    let (status, body) = post(&fx.app, "/api/rooms/983/leads/k-pending/reject").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dedupe_key"], "k-pending");
    assert_eq!(body["status"], "rejected", "{body}");
    assert_eq!(body["changed"], true, "{body}");
    assert_eq!(body["reject_chips"], Value::Array(vec![]), "{body}");
    assert_eq!(body["reject_note"], "", "{body}");
    let after = ledger_rows(&fx.data_root);
    assert_eq!(after.len(), 5, "行数不变");
    let flipped = after.iter().find(|r| r.dedupe_key == "k-pending").unwrap();
    assert_eq!(flipped.status, LeadStatus::Rejected);
    assert!(flipped.reject_chips.is_empty(), "全空拒因留档为空");
    assert!(flipped.reject_note.is_empty(), "全空拒因 note 为空");
    let strip = |rows: &[LedgerRow]| {
        rows.iter()
            .filter(|r| r.dedupe_key != "k-pending")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&before), strip(&after), "其余行逐字段原样：{after:?}");
}

/// 钉②拒因随体落库：chips 留档 JSON 且 note 原样（端点 trim 规范化后落账）。
#[tokio::test(flavor = "multi_thread")]
async fn reject_records_chips_and_note() {
    let fx = fixture();
    let (status, body) = post_json(
        &fx.app,
        "/api/rooms/983/leads/k-pending/reject",
        r#"{"chips":["太泛","做不了"],"note":" 主播不玩这品类  "}"#,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["changed"], true, "{body}");
    let flipped = ledger_rows(&fx.data_root)
        .into_iter()
        .find(|r| r.dedupe_key == "k-pending")
        .unwrap();
    assert_eq!(
        flipped.reject_chips,
        vec!["太泛".to_string(), "做不了".to_string()]
    );
    // note 端点层 trim（规格外自裁：存储/回显/重放比较用同一规范化值）。
    assert_eq!(flipped.reject_note, "主播不玩这品类");
    // 同参重放：已是终态、参数相同 → 幂等（changed=false、表行不动）。
    let settled = ledger_rows(&fx.data_root);
    let (status, replay) = post_json(
        &fx.app,
        "/api/rooms/983/leads/k-pending/reject",
        r#"{"chips":["太泛","做不了"],"note":"主播不玩这品类"}"#,
    )
    .await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["changed"], false, "{replay}");
    assert_eq!(replay["status"], "rejected", "{replay}");
    assert_eq!(
        ledger_rows(&fx.data_root),
        settled,
        "幂等重放不得写表（行面逐字段不动）"
    );
}

/// 钉③先翻转再空体重放 = 幂等；已 rejected 且带相异非空参 → 422 讲规则。
#[tokio::test(flavor = "multi_thread")]
async fn reject_replay_is_idempotent_and_rewrite_attempt_is_422() {
    let fx = fixture();
    let (status, first) = post(&fx.app, "/api/rooms/983/leads/k-pending/reject").await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["changed"], true);
    let settled = ledger_rows(&fx.data_root);
    // 空体重放：幂等
    let (status, empty_replay) = post(&fx.app, "/api/rooms/983/leads/k-pending/reject").await;
    assert_eq!(status, 200, "{empty_replay}");
    assert_eq!(empty_replay["changed"], false, "{empty_replay}");
    assert_eq!(ledger_rows(&fx.data_root), settled, "空体重放不写表");
    // 相异非空参 → 422（终态留档不可覆盖，错文讲规则）
    let (status, rewrite) = post_json(
        &fx.app,
        "/api/rooms/983/leads/k-pending/reject",
        r#"{"chips":["已知道"],"note":"补注"}"#,
    )
    .await;
    assert_eq!(status, 422, "{rewrite}");
    assert!(
        rewrite["error"]
            .as_str()
            .is_some_and(|s| s.contains("rejected") && s.contains("覆盖")),
        "{rewrite}"
    );
    assert_eq!(ledger_rows(&fx.data_root), settled, "422 面不得碰表");
    // 已 rejected 行不带参直调也幂等
    let (status, on_rejected) = post(&fx.app, "/api/rooms/983/leads/k-rejected/reject").await;
    assert_eq!(status, 200, "{on_rejected}");
    assert_eq!(on_rejected["changed"], false, "{on_rejected}");
}

/// 钉④不存在 = 404：未知 lead_id / 错房间 / 穿透形 id，表均不动。
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
    let (status, _body) = post(&fx.app, "/api/rooms/999/leads/k-pending/approve").await;
    assert_eq!(status, 404, "错房间 → 404");
    assert_eq!(ledger_rows(&fx.data_root), before, "404 面不得碰表");
}

/// 钉⑤非法迁移 = 422：consumed/approved/deferred 源态皆拒（拒绝单行道的唯一
/// 源态是 pending_approval），错文讲规则 + 当前状态，表行逐字段不动。
#[tokio::test(flavor = "multi_thread")]
async fn reject_rejects_illegal_source_states_422() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    for (lead_id, current) in [
        ("k-consumed", "consumed"),
        ("k-approved", "approved"),
        ("k-deferred", "deferred"),
    ] {
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

/// 钉⑥拒因参数校验 = 422：chips 出白名单 / 超 4 项 / note 超 80 字 / 体坏 JSON /
/// 体非对象 / chips 非数组 / note 非字符串——任一脱靶都不落账。
#[tokio::test(flavor = "multi_thread")]
async fn reject_validates_reason_params_422() {
    let fx = fixture();
    let before = ledger_rows(&fx.data_root);
    let mut cases: Vec<(String, &str)> = vec![
        (r#"{"chips":["垃圾"]}"#.into(), "白名单"),
        (
            r#"{"chips":["太泛","不对路","已知道","做不了","太泛"]}"#.into(),
            "最多",
        ),
        (r#"{不是 json"#.into(), "JSON"),
        (r#""错误形态""#.into(), "对象"),
        (r#"{"chips":"太泛"}"#.into(), "数组"),
        (r#"{"note":42}"#.into(), "字符串"),
        // 超长 note 用例单独构造（repeat 不能在字面量里用）。
        (format!(r#"{{"note":"{}"}}"#, "长".repeat(81)), "最多"),
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
}
