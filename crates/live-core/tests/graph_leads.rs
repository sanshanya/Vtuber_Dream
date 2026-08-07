//! G2（design §9.2 行 254）JSONL→表迁移钉团：
//!
//! - schema v7：`discovery_leads` 表（LedgerRow 全字段 + first_seen_run_id→graph_runs
//!   外键）+ `episodes.lead_id` 列；v6→v7 就地升级原数据完好（旧库不兼容政策
//!   至此让位：纯增量迁移零成本，照 GRAPH_SCHEMA_VERSION 机制升格）。
//! - 表面语义：dedupe 唯一键违例被拒；record 幂等；状态机单行道；
//!   JSONL vs 表读面 parity（同一 LedgerRow 向量、同一 summary_line）。
mod common;

use std::path::{Path, PathBuf};

use serde_json::json;

use live_core::graph::store::{GRAPH_SCHEMA_VERSION, Store};
use live_core::leads::{self, LeadStatus, LedgerRow};
use live_core::models::Lead;

// ---------------------------------------------------------------------------
// channelSetup
// ---------------------------------------------------------------------------

/// 内存库 + 一条 graph_run（first_seen_run_id 的外键锚；FK=ON 全局纪律）。
fn mem_store() -> Store {
    let store = Store::open(Path::new(":memory:")).expect("mem store");
    store
        .begin_run_fixed("run:a", "2026-08-05T00:00:00+00:00", "m")
        .unwrap();
    store
}

fn lead(lead_type: &str, locator: &str) -> Lead {
    Lead {
        lead_type: lead_type.to_string(),
        locator: locator.to_string(),
        motivation: "m".to_string(),
        expected_signal: "s".to_string(),
        priority: "high".to_string(),
        evidence_ids: vec!["e1".to_string()],
    }
}

fn row(key: &str, lead_type: &str, status: LeadStatus) -> LedgerRow {
    LedgerRow {
        dedupe_key: key.into(),
        lead_type: lead_type.into(),
        locator: format!("loc-{key}"),
        motivation: "m".into(),
        expected_signal: "s".into(),
        priority: "high".into(),
        evidence_ids: vec!["e1".into()],
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

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("g2-leads-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
// ---------------------------------------------------------------------------
// 钉①：schema v7 新库——discovery_leads 表 + episodes.lead_id 列
// ---------------------------------------------------------------------------

#[test]
fn v7_fresh_schema_has_discovery_leads_and_episode_lead_id() {
    // 版本钉：本批升格 v6→v7；后续纯增量版本（如 §8.6 维护面的 v8）沿升格通道上叠，
    // 本钉只钉「不低于 7」的下界，不与后续 bump 互相锁死。
    // 编译期断言（inline const）：常量比较恒真会触 assertions_on_constants，
    // 故须在 const 块内评估——回退 <7 直接编译不过，运行时零成本。
    const { assert!(GRAPH_SCHEMA_VERSION >= 9) }
    let store = mem_store();
    let leads_cols: Vec<String> = store
        .conn
        .prepare("PRAGMA table_info(discovery_leads)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for col in [
        "dedupe_key",
        "lead_type",
        "locator",
        "motivation",
        "expected_signal",
        "priority",
        "evidence_ids_json",
        "viewer_id",
        "first_seen_run_id",
        "created_at",
        "status",
        "yield_count",
        "resolution_note",
        "reject_chips_json",
        "reject_note",
    ] {
        assert!(
            leads_cols.iter().any(|c| c == col),
            "discovery_leads 缺列 {col}: {leads_cols:?}"
        );
    }
    let ep_cols: Vec<String> = store
        .conn
        .prepare("PRAGMA table_info(episodes)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        ep_cols.iter().any(|c| c == "lead_id"),
        "episodes 缺 lead_id: {ep_cols:?}"
    );
}

// ---------------------------------------------------------------------------
// 钉②：v6→v7 就地升级——旧库原数据完好、版本升格、新表/新列就绪
// ---------------------------------------------------------------------------

/// 手工建 v6 库：沿用 v6 的 episodes DDL（无 lead_id）+ 最小数据面，
/// user_version=6；Store::open 触就地升级。
#[test]
fn v6_db_upgrades_in_place_and_preserves_data() {
    let dir = tmp_dir("v6");
    let path = dir.join("perception.sqlite3");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE graph_runs (
                run_id TEXT PRIMARY KEY,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                failed_at TEXT,
                failure_json TEXT,
                model TEXT
            );
            CREATE TABLE nodes (
                node_id TEXT PRIMARY KEY, node_type TEXT NOT NULL, name TEXT NOT NULL,
                properties_json TEXT NOT NULL DEFAULT '{}', source_kind TEXT NOT NULL,
                first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL
            );
            CREATE TABLE episodes (
                episode_id TEXT PRIMARY KEY, viewer_id TEXT NOT NULL, source TEXT NOT NULL,
                event_type TEXT NOT NULL, observed_at TEXT NOT NULL, published_at TEXT,
                title TEXT, url TEXT, bvid TEXT, fields_json TEXT NOT NULL,
                platform_facts_json TEXT NOT NULL, content_hash TEXT NOT NULL,
                first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL
            );
            CREATE TABLE mentions (
                mention_id TEXT PRIMARY KEY,
                episode_id TEXT NOT NULL REFERENCES episodes(episode_id),
                viewer_id TEXT NOT NULL, field_path TEXT NOT NULL, text TEXT NOT NULL,
                start_offset INTEGER NOT NULL, end_offset INTEGER NOT NULL,
                mention_type TEXT NOT NULL, origin TEXT NOT NULL,
                proposed_entity_name TEXT NOT NULL, proposed_entity_type TEXT NOT NULL,
                confidence REAL NOT NULL, run_id TEXT NOT NULL REFERENCES graph_runs(run_id),
                created_at TEXT NOT NULL
            );
            CREATE TABLE entities (
                entity_id TEXT PRIMARY KEY, canonical_name TEXT NOT NULL,
                normalized_name TEXT NOT NULL, entity_type TEXT NOT NULL, description TEXT,
                source_kind TEXT NOT NULL, properties_json TEXT NOT NULL DEFAULT '{}',
                first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL
            );
            CREATE TABLE entity_aliases (
                alias_key TEXT NOT NULL,
                entity_id TEXT NOT NULL REFERENCES entities(entity_id),
                alias TEXT NOT NULL, source_kind TEXT NOT NULL, confidence REAL NOT NULL,
                created_at TEXT NOT NULL, PRIMARY KEY(alias_key, entity_id)
            );
            CREATE TABLE edges (
                edge_id TEXT PRIMARY KEY,
                source_id TEXT NOT NULL REFERENCES nodes(node_id),
                predicate TEXT NOT NULL,
                target_id TEXT NOT NULL REFERENCES nodes(node_id),
                properties_json TEXT NOT NULL DEFAULT '{}', source_kind TEXT NOT NULL,
                confidence REAL, evidence_json TEXT NOT NULL DEFAULT '[]',
                valid_from TEXT NOT NULL, valid_to TEXT,
                first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                run_id TEXT REFERENCES graph_runs(run_id),
                viewer_id TEXT NOT NULL DEFAULT ''
            );
            INSERT INTO graph_runs(run_id, started_at, model)
            VALUES('run:v6','t0','m'), ('run:a','t0','m');
            INSERT INTO nodes(node_id, node_type, name, source_kind, first_seen_at, last_seen_at)
            VALUES('viewer:1001','Viewer','老观众','platform_fact','t0','t0');
            INSERT INTO episodes(episode_id, viewer_id, source, event_type, observed_at,
                                 fields_json, platform_facts_json, content_hash,
                                 first_seen_at, last_seen_at)
            VALUES('ep:v6','viewer:1001','profile_dynamic_note','note','t0','[]','{}','h','t0','t0');
            PRAGMA user_version = 6;
            "#,
        )
        .unwrap();
    }
    let store = Store::open(&path).expect("v6 库必须就地升级成功");
    let version: i64 = store
        .conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(
        version, GRAPH_SCHEMA_VERSION,
        "升级后 user_version 必须 = GRAPH_SCHEMA_VERSION"
    );
    // v6→v9 连锁段就绪——拒因两列在升级库上可按 NULL 态写出/读回
    let rejects_cols: Vec<String> = store
        .conn
        .prepare("PRAGMA table_info(discovery_leads)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for col in ["reject_chips_json", "reject_note"] {
        assert!(
            rejects_cols.iter().any(|c| c == col),
            "v6 连锁升级后 discovery_leads 缺列 {col}: {rejects_cols:?}"
        );
    }
    // 原数据完好
    let node_name: String = store
        .conn
        .query_row(
            "SELECT name FROM nodes WHERE node_id='viewer:1001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(node_name, "老观众");
    let ep_viewer: String = store
        .conn
        .query_row(
            "SELECT viewer_id FROM episodes WHERE episode_id='ep:v6'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ep_viewer, "viewer:1001");
    // 旧 episode 的 lead_id 默认 NULL
    let lead: Option<String> = store
        .conn
        .query_row(
            "SELECT lead_id FROM episodes WHERE episode_id='ep:v6'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lead, None);
    // 新表就绪可写
    store
        .insert_lead_rows(&[&row("k1", "video", LeadStatus::PendingApproval)], false)
        .unwrap();
    assert_eq!(store.lead_rows().unwrap().len(), 1);
    // 升级幂等：再开同库不再迁移、不报错
    drop(store);
    let again = Store::open(&path).expect("v7 库重开成功");
    assert_eq!(again.lead_rows().unwrap().len(), 1);
}

/// 旧于 v6 的库仍沿用「删除重跑」政策：报错且不剪表。
#[test]
fn pre_v6_db_still_rejected_with_outdated_error() {
    let dir = tmp_dir("v5");
    let path = dir.join("perception.sqlite3");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE sentinel(x TEXT); PRAGMA user_version = 5;")
            .unwrap();
    }
    let Err(err) = Store::open(&path) else {
        panic!("v5 及以下仍必须被拒");
    };
    assert!(
        err.to_string().contains("outdated graph database"),
        "v5 及以下仍走旧政策错误面：{err}"
    );
}

// ---------------------------------------------------------------------------
// 钉③：record 幂等（同键重跑零增行；任意状态同键跳行）
// ---------------------------------------------------------------------------

#[test]
fn record_then_rerun_appends_zero_rows() {
    let store = mem_store();
    let batch = vec![lead("video", "BV1"), lead("search", "恋恋红莲华")];
    let first =
        leads::record_leads(&store, "u", "run:a", "2026-08-05T00:00:00+00:00", &batch).unwrap();
    assert_eq!(first, 2);
    let again =
        leads::record_leads(&store, "u", "run:a", "2026-08-05T01:00:00+00:00", &batch).unwrap();
    assert_eq!(again, 0, "同键重跑必须零增行");
    let rows = leads::read_rows(&store).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].status, LeadStatus::PendingApproval);
    assert_eq!(rows[0].first_seen_run_id, "run:a");
    let plus = leads::record_leads(&store, "u", "run:a", "t", &[lead("video", "BV2")]).unwrap();
    assert_eq!(plus, 1);
    assert_eq!(leads::read_rows(&store).unwrap().len(), 3);
}

#[test]
fn record_skips_existing_keys_at_any_state() {
    let store = mem_store();
    let batch = vec![lead("video", "BV1"), lead("search", "异环 实机")];
    leads::record_leads(&store, "u", "run:a", "t", &batch).unwrap();
    for status in [
        LeadStatus::Approved,
        LeadStatus::Consumed,
        LeadStatus::Rejected,
    ] {
        let mut rows = leads::read_rows(&store).unwrap();
        rows[0].status = status;
        rows[0].yield_count = 7;
        store.update_lead_row(&rows[0]).unwrap();
        let appended = leads::record_leads(&store, "u", "run:a", "t2", &batch).unwrap();
        assert_eq!(appended, 0, "状态 {status:?} 下同键仍必须跳行");
        assert_eq!(leads::read_rows(&store).unwrap().len(), 2);
        let back = leads::read_rows(&store).unwrap();
        assert_eq!(back[0].status, status, "record 无权回写状态机");
        assert_eq!(back[0].yield_count, 7);
    }
}

/// dedupe_key 唯一键：绕过幂等闸门的裸直写同键 → 唯一键违例被拒。
#[test]
fn dedupe_key_unique_violation_rejected() {
    let store = mem_store();
    let row_one = row("same-key", "video", LeadStatus::PendingApproval);
    store.insert_lead_rows(&[&row_one], false).unwrap();
    let mut dup = row("same-key", "search", LeadStatus::PendingApproval);
    dup.locator = "另一目标".into();
    let Err(err) = store.insert_lead_rows(&[&dup], true) else {
        panic!("dedupe_key 唯一键必须拒写");
    };
    assert!(
        err.to_string().contains("UNIQUE") || err.to_string().contains("unique"),
        "dedupe_key 唯一键必须拒写：{err}"
    );
    assert_eq!(store.lead_rows().unwrap().len(), 1);
}

/// first_seen_run_id 外键：未知 run 入账被拒（真外键纪律）。
#[test]
fn first_seen_run_id_foreign_key_enforced() {
    let store = mem_store();
    let mut foreignless = row("k-fk", "video", LeadStatus::PendingApproval);
    foreignless.first_seen_run_id = "run:不存在".into();
    let err = store.insert_lead_rows(&[&foreignless], true).unwrap_err();
    assert!(
        err.to_string().contains("FOREIGN KEY") || err.to_string().contains("foreign key"),
        "first_seen_run_id 必须真外键：{err}"
    );
}

// ---------------------------------------------------------------------------
// 钉④：状态机（表上 approve）
// ---------------------------------------------------------------------------

#[test]
fn approve_transition_over_table() {
    let store = mem_store();
    let pending = row("k-p", "video", LeadStatus::PendingApproval);
    store.insert_lead_rows(&[&pending], false).unwrap();
    let mut current = store.lead_row("k-p").unwrap().unwrap();
    assert!(leads::approve_transition(current.status).unwrap());
    current.status = LeadStatus::Approved;
    store.update_lead_row(&current).unwrap();
    let back = store.lead_row("k-p").unwrap().unwrap();
    assert_eq!(back.status, LeadStatus::Approved);
    // 幂等重放：approve_transition(Approved) = Ok(false)
    assert!(!leads::approve_transition(back.status).unwrap());
    for status in [LeadStatus::Consumed, LeadStatus::Rejected] {
        assert!(
            leads::approve_transition(status).is_err(),
            "{status:?} 必须拒"
        );
    }
}

// ---------------------------------------------------------------------------
// 钉⑤：JSONL 一次性导入 + 归档 .bak + 幂等（二次零变化 / 重导入零新行）
// ---------------------------------------------------------------------------
// 钉⑥：读面 parity——JSONL 解析与表读出全等（LedgerRow 向量 + summary_line）
// ---------------------------------------------------------------------------
// 钉⑦：episodes.lead_id——挂链写入面
// ---------------------------------------------------------------------------

fn episode(episode_id: &str) -> live_core::episodes::Episode {
    live_core::episodes::Episode {
        episode_id: episode_id.into(),
        viewer_id: "viewer:1001".into(),
        source: "profile_dynamic_note".into(),
        event_type: "note".into(),
        observed_at: "t".into(),
        published_at: String::new(),
        title: String::new(),
        url: String::new(),
        bvid: String::new(),
        fields: vec![],
        platform_facts: json!({}),
    }
}

#[test]
fn episode_lead_linkage_write_face() {
    let store = mem_store();
    // 普通 episode（非线索出产）→ lead_id NULL
    store.upsert_episode(&episode("ep:plain")).unwrap();
    let plain: Option<String> = store
        .conn
        .query_row(
            "SELECT lead_id FROM episodes WHERE episode_id='ep:plain'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(plain, None);
    // 线索出产 episode → lead_id 挂上 dedupe_key
    store
        .insert_lead_rows(&[&row("k-link", "video", LeadStatus::Approved)], false)
        .unwrap();
    store
        .upsert_episode_with_lead(&episode("ep:from-lead"), Some("k-link"))
        .unwrap();
    let linked: Option<String> = store
        .conn
        .query_row(
            "SELECT lead_id FROM episodes WHERE episode_id='ep:from-lead'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked.as_deref(), Some("k-link"));
    // 裸 upsert 复检幂等路径不抹既有 lead_id（冲突走的是 last_seen 刷新臂）
    store.upsert_episode(&episode("ep:from-lead")).unwrap();
    let still: Option<String> = store
        .conn
        .query_row(
            "SELECT lead_id FROM episodes WHERE episode_id='ep:from-lead'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still.as_deref(), Some("k-link"), "幂等复检不得抹挂链");
    // 假外键：不存在的 lead → 拒写
    let err = store
        .upsert_episode_with_lead(&episode("ep:bad"), Some("k-不存在"))
        .unwrap_err();
    assert!(
        err.to_string().contains("FOREIGN KEY") || err.to_string().contains("foreign key"),
        "episodes.lead_id 必须真外键：{err}"
    );
}

// ---------------------------------------------------------------------------
// 事实密度 annex 钉团（迭代细则 v1 §1 验收钉三连）：
// ①零新增时 annex 与前轮逐字节一致；
// ②每行可回链 lead→episode（行尾 anchor = episode_id 尾 12，且 real 查得回）；
// ③总长度上限：超出即行边界断 + 封顶句（防账本滚大吞 prompt）。
// ---------------------------------------------------------------------------

fn annex_episode(episode_id: &str, viewer: &str, title: &str) -> live_core::episodes::Episode {
    let mut e = episode(episode_id);
    e.viewer_id = viewer.to_string();
    e.title = title.to_string();
    e
}

#[test]
fn p0_3_annex_zero_new_byte_identical_and_link_pinnable() {
    let store = mem_store();
    let lead_consumed = LedgerRow {
        dedupe_key: "k-cons".into(),
        lead_type: "viewer".into(),
        locator: "uid:3546".into(),
        motivation: "m".into(),
        expected_signal: "s".into(),
        priority: "high".into(),
        evidence_ids: vec![],
        viewer_id: "1001".into(),
        first_seen_run_id: "run:a".into(),
        created_at: "t".into(),
        status: LeadStatus::Consumed,
        yield_count: 2,
        resolution_note: String::new(),
        reject_chips: Vec::new(),
        reject_note: String::new(),
    };
    let pending = row("k-pend", "viewer", LeadStatus::PendingApproval);
    store
        .insert_lead_rows(&[&lead_consumed, &pending], false)
        .expect("seed leads");
    store
        .upsert_episode_with_lead(&annex_episode("ep:a", "1001", "动态一条"), Some("k-cons"))
        .unwrap();
    store
        .upsert_episode_with_lead(
            &annex_episode("ep:x-longer-than-12-tail", "1001", "唱歌回切片"),
            Some("k-cons"),
        )
        .unwrap();
    // 注：annex.go 的排序键是 episode_id 字典序 ASC——
    // 「末条」= ep:x-longer-than-12-tail（语义仅排序确定性，不含时序表态）。

    let rows = leads::read_rows(&store).unwrap();
    // 钉①：同一库两次取 annex → 逐字节一致
    let a = leads::consumed_annex_lines(&store, &rows)
        .unwrap()
        .join("\n");
    let b = leads::consumed_annex_lines(&store, &rows)
        .unwrap()
        .join("\n");
    assert_eq!(a, b, "零新增时 annex 必须逐字节一致");
    assert!(a.contains("[lead_fact]"), "{a}");
    assert!(a.contains("viewer:uid:3546"), "{a}");
    assert!(a.contains("总证据 2"), "{a}");
    assert!(a.contains("唱歌回切片"), "末条摘要姿: {a}");
    // 钉②：回链锚 = episode_id 尾 12 （ep:x-longer-than-12-tail → 尾 12 = or-than-12-tail）
    let tail: String = "ep:x-longer-than-12-tail"
        .chars()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert!(a.contains(&tail), "annex 必含 episode 回链 {tail}: {a}");
    let verified = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE episode_id LIKE '%' || ?",
            rusqlite::params![tail],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(verified, 1, "回链锚必须能查回单一 episode");
    // 观众画像计数：1001 刷 2 条
    assert!(a.contains("1001+2"), "{a}");
    // pending 入账不进 annex（annex 是事实密度，不是状态列表）
    assert!(!a.contains("k-pend"), "{a}");
}

#[test]
fn p0_3_annex_monotone_drift_only_on_new_consumed() {
    let store = mem_store();
    let mut rows = leads::read_rows(&store).unwrap();
    let base = leads::consumed_annex_lines(&store, &rows)
        .unwrap()
        .join("\n");
    assert_eq!(base.trim(), "", "零账本 → 零 annex");

    let a1 = LedgerRow {
        dedupe_key: "k-1".into(),
        lead_type: "viewer".into(),
        locator: "uid:1".into(),
        status: LeadStatus::Consumed,
        ..row("k-1", "viewer", LeadStatus::Consumed)
    };
    store.insert_lead_rows(&[&a1], false).unwrap();
    store
        .upsert_episode_with_lead(&annex_episode("ep:1", "1001", "X"), Some("k-1"))
        .unwrap();
    rows = leads::read_rows(&store).unwrap();
    let step1 = leads::consumed_annex_lines(&store, &rows)
        .unwrap()
        .join("\n");
    assert!(step1.contains("uid:1"), "{step1}");

    // 再插入一条 pending（不是新 consumed）→ annex 逐字节不变（单调漂移律）
    let p1 = row("k-p", "viewer", LeadStatus::PendingApproval);
    store.insert_lead_rows(&[&p1], false).unwrap();
    rows = leads::read_rows(&store).unwrap();
    let step2 = leads::consumed_annex_lines(&store, &rows)
        .unwrap()
        .join("\n");
    assert_eq!(step1, step2, "pending 入账不得引起 annex 漂移");

    // 新 consumed → annex 真实漂移（正变化：新增 consumed 事件）
    let c2 = LedgerRow {
        dedupe_key: "k-2".into(),
        lead_type: "public_channel".into(),
        locator: "uid:2".into(),
        status: LeadStatus::Consumed,
        ..row("k-2", "public_channel", LeadStatus::Consumed)
    };
    store.insert_lead_rows(&[&c2], false).unwrap();
    rows = leads::read_rows(&store).unwrap();
    let step3 = leads::consumed_annex_lines(&store, &rows)
        .unwrap()
        .join("\n");
    assert_ne!(
        step2.trim(),
        step3.trim(),
        "新增 consumed 必须使 annex 产生漂移（单调：只随 consumed 事件变）"
    );
    assert!(step3.contains("uid:2"), "{step3}");
}

#[test]
fn p0_3_annex_total_chars_cap_and_line_boundary_cut() {
    let store = mem_store();
    // 三条 consumed lead，每条约 137 字（locator 24 截 + 观众块 + 40 字摘 + 回链锚）：
    // 前两行 ≈274 字进账，第三行拱过 360 闸 → 行边界断 + 封顶句。
    for (key, uid) in [("k-a", "观众甲"), ("k-b", "观众乙"), ("k-c", "观众丙")] {
        let row_big = LedgerRow {
            dedupe_key: key.into(),
            lead_type: "public_channel".into(),
            locator: format!("uid:anchor【{uid}】长定位"),
            status: LeadStatus::Consumed,
            ..row(key, "public_channel", LeadStatus::Consumed)
        };
        store.insert_lead_rows(&[&row_big], false).unwrap();
        let e = annex_episode(
            &format!("ep:{key}-tail-anchor"),
            uid,
            &"这是一个相当丰满的摘要切片文本填充到四十字符上限来拉长行".repeat(3),
        );
        store.upsert_episode_with_lead(&e, Some(key)).unwrap();
    }
    let rows = leads::read_rows(&store).unwrap();
    let lines = leads::consumed_annex_lines(&store, &rows).unwrap();
    let annex = lines.join("\n");
    assert!(annex.contains("封因为其截断"), "超闸必须落封顶句: {annex}");
    // 行边界断：截断句必为最后一行，且前半行数是整段前缀
    let last = lines.last().unwrap();
    assert!(last.contains("封因为其截断"), "{last}");
    // 总量可容：含封顶句在内的全文仍压在闸+封顶句袋（不超 500 字——防滚大之本）
    assert!(
        annex.chars().count() <= 500,
        "annex 总长度不得超过段包，实测 {}",
        annex.chars().count()
    );
}

// ---------------------------------------------------------------------------
// reject 留档——chips/note 列读写、NULL=全空合法态、旧 JSONL 兼容
// ---------------------------------------------------------------------------

/// insert/update 双写点 + 读回：chips 走 JSON 文本列、note 走宽松文本列；
/// 全空拒因 → 两列 NULL（不是空串/空数组）——规格的「全空合法：NULL/NULL」。
#[test]
fn reject_fields_roundtrip_and_null_all_empty() {
    let store = mem_store();
    let mut rejected = row("k1", "video", LeadStatus::Rejected);
    rejected.reject_chips = vec!["太泛".into(), "做不了".into()];
    rejected.reject_note = "重复提出，暂缓".into();
    store.insert_lead_rows(&[&rejected], false).unwrap();
    let all_empty = row("k2", "creator", LeadStatus::Rejected);
    store.insert_lead_rows(&[&all_empty], false).unwrap();

    let rows: Vec<_> = store
        .conn
        .prepare("SELECT dedupe_key, reject_chips_json, reject_note FROM discovery_leads")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let by_key = |key: &str| rows.iter().find(|(k, _, _)| k == key).unwrap();
    let (_, chips_json, note) = by_key("k1");
    assert_eq!(chips_json.as_deref(), Some("[\"太泛\",\"做不了\"]"));
    assert_eq!(note.as_deref(), Some("重复提出，暂缓"));
    assert_eq!(by_key("k2").1, None, "全空拒因 chip 面必须落 NULL");
    assert_eq!(by_key("k2").2, None, "全空拒因注记面必须落 NULL");

    // 读回面：None → 空数组/空串（LedgerRow 形态与 serde default 面同源）
    let k1 = leads::read_rows(&store)
        .unwrap()
        .iter()
        .find(|r| r.dedupe_key == "k1")
        .unwrap()
        .clone();
    assert_eq!(
        k1.reject_chips,
        vec!["太泛".to_string(), "做不了".to_string()]
    );
    assert_eq!(k1.reject_note, "重复提出，暂缓");
    let k2 = leads::read_rows(&store)
        .unwrap()
        .iter()
        .find(|r| r.dedupe_key == "k2")
        .unwrap()
        .clone();
    assert_eq!(k2.reject_chips, Vec::<String>::new());
    assert_eq!(k2.reject_note, "");

    // update 写回点同口径：改拒因再读回全等；清空 → 回到 NULL 空态
    let mut updated = k1.clone();
    updated.reject_chips = vec![]; // 清空回到合法全空
    updated.reject_note = String::new();
    store.update_lead_row(&updated).unwrap();
    let (chips_json, note) = {
        let mut stmt = store
            .conn
            .prepare(
                "SELECT reject_chips_json, reject_note FROM discovery_leads WHERE dedupe_key='k1'",
            )
            .unwrap();
        stmt.query_row([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .unwrap()
    };
    assert_eq!(chips_json, None, "update 清空后 chip 列回 NULL");
    assert_eq!(note, None, "update 清空后注记列回 NULL");
}
