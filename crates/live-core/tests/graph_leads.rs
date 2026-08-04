//! G2（design §9.2 行 254）JSONL→表迁移钉团：
//!
//! - schema v7：`discovery_leads` 表（LedgerRow 全字段 + first_seen_run_id→graph_runs
//!   外键）+ `episodes.lead_id` 列；v6→v7 就地升级原数据完好（旧库不兼容政策
//!   至此让位：纯增量迁移零成本，照 GRAPH_SCHEMA_VERSION 机制升格）。
//! - 迁移手段：`leads::migrate_jsonl` 把既有 leads.jsonl 一次性导入表（幂等——
//!   dedupe_key 唯一键，重导入零新行），文件归档 `leads.jsonl.bak`（可回滚，不删除）。
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
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("g2-leads-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jsonl(dir: &Path, rows: &[LedgerRow]) {
    let text = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(leads::ledger_path(dir), text).unwrap();
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
    const { assert!(GRAPH_SCHEMA_VERSION >= 7) }
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
        "升级后 user_version 必须 = 7"
    );
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
        LeadStatus::Deferred,
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
    for status in [
        LeadStatus::Consumed,
        LeadStatus::Rejected,
        LeadStatus::Deferred,
    ] {
        assert!(
            leads::approve_transition(status).is_err(),
            "{status:?} 必须拒"
        );
    }
}

// ---------------------------------------------------------------------------
// 钉⑤：JSONL 一次性导入 + 归档 .bak + 幂等（二次零变化 / 重导入零新行）
// ---------------------------------------------------------------------------

#[test]
fn migrate_jsonl_imports_archives_and_is_idempotent() {
    let store = mem_store();
    let dir = tmp_dir("migrate");
    let rows = vec![
        row("k1", "video", LeadStatus::PendingApproval),
        row("k2", "search", LeadStatus::Approved),
        row("k3", "creator", LeadStatus::Consumed),
    ];
    write_jsonl(&dir, &rows);

    // 一次导入：3 行入表、jsonl 归档为 .bak（原文件消失、可回滚副本在场）
    let imported = leads::migrate_jsonl(&store, &dir).unwrap();
    assert_eq!(imported, 3);
    assert!(!leads::ledger_path(&dir).exists(), "jsonl 必须被归档搬走");
    let bak = dir.join("leads.jsonl.bak");
    assert!(bak.exists(), ".bak 归档必须在场（可回滚）");
    let table_rows = leads::read_rows(&store).unwrap();
    assert_eq!(table_rows.len(), 3);

    // 幂等①：二次迁移零变化（无 jsonl 可导、表行数不变、bak 不被覆盖）
    let bak_bytes = std::fs::read(&bak).unwrap();
    let again = leads::migrate_jsonl(&store, &dir).unwrap();
    assert_eq!(again, 0, "二次迁移必须零变化");
    assert_eq!(leads::read_rows(&store).unwrap().len(), 3);
    assert_eq!(std::fs::read(&bak).unwrap(), bak_bytes, "bak 不动");

    // 幂等②：人为复原 jsonl（同 3 行 + 1 新行）→ 重导入只进新行
    let mut replay = rows.clone();
    replay.push(row("k4", "video", LeadStatus::PendingApproval));
    write_jsonl(&dir, &replay);
    let delta = leads::migrate_jsonl(&store, &dir).unwrap();
    assert_eq!(delta, 1, "dedupe_key 唯一键：同键零新行，只进 k4");
    let table_rows = leads::read_rows(&store).unwrap();
    assert_eq!(table_rows.len(), 4);
    let k2 = table_rows.iter().find(|r| r.dedupe_key == "k2").unwrap();
    assert_eq!(k2.status, LeadStatus::Approved, "迁移保留原状态机位");
    let k3 = table_rows.iter().find(|r| r.dedupe_key == "k3").unwrap();
    assert_eq!(k3.status, LeadStatus::Consumed);
}

/// 迁移守卫：jsonl 含坏行 → Err 响铃 + 文件原地不动（不推进归档）。
#[test]
fn migrate_jsonl_with_bad_line_rings_and_stays_put() {
    let store = mem_store();
    let dir = tmp_dir("bad-migrate");
    let good = row("k1", "video", LeadStatus::PendingApproval);
    let text = format!(
        "{}\nnot json at all\n",
        serde_json::to_string(&good).unwrap()
    );
    std::fs::write(leads::ledger_path(&dir), &text).unwrap();
    let err = leads::migrate_jsonl(&store, &dir).unwrap_err();
    assert!(err.to_string().contains("不可解析"), "{err}");
    assert_eq!(
        std::fs::read_to_string(leads::ledger_path(&dir)).unwrap(),
        text,
        "坏行在场时账本文本逐字节不动"
    );
    assert_eq!(
        leads::read_rows(&store).unwrap().len(),
        0,
        "守卫拒下不导半份"
    );
}

// ---------------------------------------------------------------------------
// 钉⑥：读面 parity——JSONL 解析与表读出全等（LedgerRow 向量 + summary_line）
// ---------------------------------------------------------------------------

#[test]
fn parity_jsonl_vs_table_read_face() {
    let store = mem_store();
    let dir = tmp_dir("parity");
    let mut rows = vec![
        row("k1", "video", LeadStatus::PendingApproval),
        row("k2", "search", LeadStatus::Approved),
        row("k3", "creator", LeadStatus::Consumed),
    ];
    rows[2].yield_count = 5;
    write_jsonl(&dir, &rows);
    // JSONL 解析面（迁移器同源的逐行 serde）
    let jsonl_rows =
        leads::parse_ledger_text(&std::fs::read_to_string(leads::ledger_path(&dir)).unwrap())
            .unwrap();
    leads::migrate_jsonl(&store, &dir).unwrap();
    let table_rows = leads::read_rows(&store).unwrap();
    assert_eq!(table_rows, jsonl_rows, "表读面必须与 JSONL 读面逐行全等");
    assert_eq!(
        leads::summary_line(&table_rows, None),
        leads::summary_line(&jsonl_rows, None),
        "摘要段必须两源同输出"
    );
    assert_eq!(
        leads::summary_line(&table_rows, Some("u")),
        leads::summary_line(&jsonl_rows, Some("u")),
        "viewer 作用域摘要段同输出"
    );
}

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
