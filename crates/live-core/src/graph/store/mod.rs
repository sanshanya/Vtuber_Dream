//! 时序属性图存储（移植 Python `graph.py` 的 GraphRepository）。
//!
//! schema v6（设计文档 §8.1）相对 v5 的**显式升级**：
//! 1. 真实外键（edges→nodes、edges.run_id→graph_runs、mentions→episodes、
//!    entity_aliases→entities）；
//! 2. edges 增加 `viewer_id` 列（close_missing_viewer_semantic_edges 的行为判定键，
//!    等价 v5 的 json_extract；properties_json 原样保留，仓库字节兼容）；
//! 3. TARGETS/ABOUT 等 action 边必带 confidence（见 build.rs）。
//!
//! schema v7（设计文档 §9.2 行 254，G2 JSONL→表迁移）：
//! 1. 新增 `discovery_leads` 表——M4.x `leads.jsonl` 账本字段照抄 +
//!    `first_seen_run_id` 真外键（→graph_runs），`dedupe_key` 主键即唯一键；
//! 2. episodes 增加 `lead_id` 列（→discovery_leads 外键，线索出产的溯源链）；
//! 3. v6→v7 是**首个就地升级**：纯增量（加表加列）无损迁移，user_version 升格，
//!    v6 以下旧库仍沿用“删除重跑”政策。
//!
//! schema v8（设计文档 §8.6 图维护操作）：
//! 1. graph_runs 增 `kind`（pipeline|maintenance）与 `detail_json`（维护操作全参数，
//!    可回放审计）——run「类型」此前只有隐式单类（pipeline），§8.6 的 MAINTENANCE
//!    run 是第二个类型，按规格出生；
//! 2. v7→v8 纯增量就地升级（graph_runs 补两列），沿用升格通道；
//!    v6 连锁两段（v6→v7→v8），v6 以下仍吃“删除重跑”政策。
//!
//! schema v9（D9/R2-批6 leads 审批增强）：discovery_leads 增 `reject_chips_json`
//! 与 `reject_note` 两列（拒绝留档面——reject 端点一击写账）；
//! 1. v8→v9 纯增量就地升级（补两列），沿用列存在性探测通道；所有 NULL 为
//!    「无拒因」的合法空态（全空允许），列不带 DEFAULT；
//! 2. v6 连锁变长三段（v6→v7→v8→v9），v6 以下仍吃“删除重跑”政策。
//!
//! 幂等语义与 v5 一致：节点属性合并保鲜、活跃边查重-合并、evidence 合并且去重保序、
//! confidence 取 max、first_seen 不变。
//!
//! 文件拆分：本文件 = schema v9 / 连接与事务 / 运行段 / 共享辅助；
//! nodes.rs / edges.rs / entities.rs / mentions.rs / leads_tbl.rs / maintenance.rs
//! 各自一类受控写入。

mod edges;
mod entities;
mod leads_tbl;
mod maintenance;
mod mentions;
mod nodes;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use crate::episodes::json_canon;

pub use maintenance::{MAINTENANCE_RUN_MODEL, MaintenanceError, MergeOutcome, SplitOutcome};
pub use mentions::mention_id_of;

pub const GRAPH_SCHEMA_VERSION: i64 = 9;
/// 最低可就地升级的源版本（v6 连锁三段到 v9；纯增量迁移）；更低的版本仍吃「删除重跑」政策。
pub const GRAPH_SCHEMA_VERSION_MIGRATABLE: i64 = 6;
/// Python GRAPH_QUERY_LIMIT：任何查询返回上限。
pub const GRAPH_QUERY_LIMIT: i64 = 500;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    Repo(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

fn repo_err<T>(message: impl Into<String>) -> Result<T> {
    Err(StoreError::Repo(message.into()))
}

/// SAVEPOINT 包裹一个受控写入单元：成功 RELEASE（autocommit 顶层即 COMMIT）；
/// 失败 ROLLBACK TO + RELEASE，中断只影响当前单元，重跑幂等。build.rs 的 apply_*
/// 与 maintenance.rs 的维护操作共用此一处实现（错误型泛化——两族错误各自就位）。
pub(crate) fn with_savepoint<T, E: From<rusqlite::Error>>(
    name: &str,
    store: &Store,
    apply: impl FnOnce() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    store.conn.execute_batch(&format!("SAVEPOINT {name}"))?;
    match apply() {
        Ok(value) => {
            store
                .conn
                .execute_batch(&format!("RELEASE SAVEPOINT {name}"))?;
            Ok(value)
        }
        Err(err) => {
            let _ = store
                .conn
                .execute_batch(&format!("ROLLBACK TO SAVEPOINT {name}"));
            let _ = store
                .conn
                .execute_batch(&format!("RELEASE SAVEPOINT {name}"));
            Err(err)
        }
    }
}

/// Python `old.update(new)` 语义：旧属性并入新 Map，新值覆盖同键；
/// 键序 = 旧序原位替换 + 新键追加（serde_json Map preserve_order 与 Python dict 一致）。
fn merge_props(new_props: Map<String, Value>, old_json: &str) -> Map<String, Value> {
    let Value::Object(old) = serde_json::from_str::<Value>(old_json).unwrap_or(Value::Null) else {
        return new_props;
    };
    let mut merged = old;
    merged.extend(new_props);
    merged
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS graph_runs (
    run_id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    failed_at TEXT,
    failure_json TEXT,
    model TEXT,
    kind TEXT NOT NULL DEFAULT 'pipeline',
    detail_json TEXT
);

CREATE TABLE IF NOT EXISTS nodes (
    node_id TEXT PRIMARY KEY,
    node_type TEXT NOT NULL,
    name TEXT NOT NULL,
    properties_json TEXT NOT NULL DEFAULT '{}',
    source_kind TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS edges (
    edge_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES nodes(node_id),
    predicate TEXT NOT NULL,
    target_id TEXT NOT NULL REFERENCES nodes(node_id),
    properties_json TEXT NOT NULL DEFAULT '{}',
    source_kind TEXT NOT NULL,
    confidence REAL,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    valid_from TEXT NOT NULL,
    valid_to TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    run_id TEXT REFERENCES graph_runs(run_id),
    viewer_id TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS episodes (
    episode_id TEXT PRIMARY KEY,
    viewer_id TEXT NOT NULL,
    source TEXT NOT NULL,
    event_type TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    published_at TEXT,
    title TEXT,
    url TEXT,
    bvid TEXT,
    fields_json TEXT NOT NULL,
    platform_facts_json TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    lead_id TEXT REFERENCES discovery_leads(dedupe_key)
);

-- G2（design §9.2 行 254）：M4.x leads.jsonl 账本的表形态——字段照抄 LedgerRow
-- （evidence_ids 落 JSON 文本列），dedupe_key 主键即幂等唯一键，
-- first_seen_run_id 真外键钉溯源锚点；status 面是 leads::status_name 的蛇形字面。
-- D9：reject_chips_json/reject_note 为拒绝留档面——chip 落 JSON 数组文本列
-- （无拒因 → NULL），note 预留宽松文本（无注记 → NULL）；全空即 NULL/NULL 合法态。
CREATE TABLE IF NOT EXISTS discovery_leads (
    dedupe_key TEXT PRIMARY KEY,
    lead_type TEXT NOT NULL,
    locator TEXT NOT NULL,
    motivation TEXT NOT NULL,
    expected_signal TEXT NOT NULL,
    priority TEXT NOT NULL,
    evidence_ids_json TEXT NOT NULL DEFAULT '[]',
    viewer_id TEXT NOT NULL,
    first_seen_run_id TEXT NOT NULL REFERENCES graph_runs(run_id),
    created_at TEXT NOT NULL,
    status TEXT NOT NULL,
    yield_count INTEGER NOT NULL DEFAULT 0,
    resolution_note TEXT NOT NULL DEFAULT '',
    reject_chips_json TEXT,
    reject_note TEXT
);

CREATE TABLE IF NOT EXISTS mentions (
    mention_id TEXT PRIMARY KEY,
    episode_id TEXT NOT NULL REFERENCES episodes(episode_id),
    viewer_id TEXT NOT NULL,
    field_path TEXT NOT NULL,
    text TEXT NOT NULL,
    start_offset INTEGER NOT NULL,
    end_offset INTEGER NOT NULL,
    mention_type TEXT NOT NULL,
    origin TEXT NOT NULL,
    proposed_entity_name TEXT NOT NULL,
    proposed_entity_type TEXT NOT NULL,
    confidence REAL NOT NULL,
    run_id TEXT NOT NULL REFERENCES graph_runs(run_id),
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entities (
    entity_id TEXT PRIMARY KEY,
    canonical_name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    description TEXT,
    source_kind TEXT NOT NULL,
    properties_json TEXT NOT NULL DEFAULT '{}',
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entity_aliases (
    alias_key TEXT NOT NULL,
    entity_id TEXT NOT NULL REFERENCES entities(entity_id),
    alias TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    confidence REAL NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(alias_key, entity_id)
);

CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id, predicate, valid_to);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id, predicate, valid_to);
CREATE INDEX IF NOT EXISTS idx_edges_source_valid ON edges(source_id, valid_to);
CREATE INDEX IF NOT EXISTS idx_edges_target_valid ON edges(target_id, valid_to);
CREATE INDEX IF NOT EXISTS idx_edges_viewer ON edges(viewer_id, source_kind);
CREATE INDEX IF NOT EXISTS idx_episodes_viewer ON episodes(viewer_id, observed_at);
CREATE INDEX IF NOT EXISTS idx_mentions_episode ON mentions(episode_id);
CREATE INDEX IF NOT EXISTS idx_entities_norm ON entities(normalized_name, entity_type);
CREATE INDEX IF NOT EXISTS idx_alias_key ON entity_aliases(alias_key);
CREATE INDEX IF NOT EXISTS idx_discovery_leads_status ON discovery_leads(status, first_seen_run_id);
CREATE INDEX IF NOT EXISTS idx_graph_runs_kind ON graph_runs(kind, completed_at);
"#;

/// v6→v7 就地升级的迁移段（SCHEMA_SQL 的增量面；新库两种路径殊途同归——
/// 本迁移用 IF NOT EXISTS / 列存在性探测保证两段 SQL 叠加幂等）。
/// 列面与 SCHEMA_SQL 同形（含 D9 的两列——老库走探测段补列，殊途同归）。
const MIGRATE_V6_TO_V7_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS discovery_leads (
    dedupe_key TEXT PRIMARY KEY,
    lead_type TEXT NOT NULL,
    locator TEXT NOT NULL,
    motivation TEXT NOT NULL,
    expected_signal TEXT NOT NULL,
    priority TEXT NOT NULL,
    evidence_ids_json TEXT NOT NULL DEFAULT '[]',
    viewer_id TEXT NOT NULL,
    first_seen_run_id TEXT NOT NULL REFERENCES graph_runs(run_id),
    created_at TEXT NOT NULL,
    status TEXT NOT NULL,
    yield_count INTEGER NOT NULL DEFAULT 0,
    resolution_note TEXT NOT NULL DEFAULT '',
    reject_chips_json TEXT,
    reject_note TEXT
);
CREATE INDEX IF NOT EXISTS idx_discovery_leads_status ON discovery_leads(status, first_seen_run_id);
"#;

/// v7→v8 就地升级段（§8.6）：graph_runs 补 kind/detail_json 两列 + kind 索引。
/// ALTER 段不幂等，列级存在性探测在 migrate_to_current 里逐列把门。
const MIGRATE_V7_TO_V8_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_graph_runs_kind ON graph_runs(kind, completed_at);
"#;

/// 活跃边的最小行（INTERESTED_IN 幂等与应用层关闭用）。
#[derive(Debug, Clone)]
pub struct ActiveEdge {
    pub edge_id: String,
    pub target_id: String,
    pub properties_json: String,
}

pub struct Store {
    /// 逃生舱：事务脚本与测试直接复用底层连接（build.rs 的 SAVEPOINT 即此用途）。
    /// 图写入优先走受控方法，零散 SQL 只使用本连接与 schema 之外的直接写仍属违约。
    pub conn: Connection,
    /// 时钟注入点：为黄金样本/回放测试固定时间；仅接受 fn 指针（不动态）。
    clock: fn() -> String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_clock(path, crate::episodes::now_iso)
    }

    pub fn open_with_clock(path: &Path, clock: fn() -> String) -> Result<Self> {
        if let Some(parent) = path.parent()
            && parent != Path::new("")
            && parent != Path::new(":memory:")
            && path != Path::new(":memory:")
        {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = if path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn, clock };
        store.schema()?;
        Ok(store)
    }

    pub fn now(&self) -> String {
        (self.clock)()
    }

    /// 轮2-R1-B2：COUNT 类标量读取公共件（原 project.rs/query.rs 各藏一份就地闭包）。
    pub fn count_scalar(&self, sql: &str, params: &[rusqlite::types::Value]) -> Result<i64> {
        let mut stmt = self.conn.prepare(sql)?;
        let value: i64 =
            stmt.query_row(rusqlite::params_from_iter(params.iter()), |row| row.get(0))?;
        Ok(value)
    }

    fn schema(&self) -> Result<()> {
        let has_schema: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if has_schema.is_some() {
            let version: i64 = self
                .conn
                .pragma_query_value(None, "user_version", |row| row.get(0))?;
            if version != GRAPH_SCHEMA_VERSION {
                // 就地升级通道：v6/v7（纯增量源版）连锁到当前版；更低旧版照旧政策吃报错。
                if (GRAPH_SCHEMA_VERSION_MIGRATABLE..GRAPH_SCHEMA_VERSION).contains(&version) {
                    self.migrate_to_current()?;
                } else {
                    return repo_err(format!(
                        "outdated graph database; delete the store and rerun (user_version={version})"
                    ));
                }
            }
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn
            .pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)?;
        Ok(())
    }

    /// 连锁就地升级：v6→v7（discovery_leads 建表 + episodes 补 lead_id）→
    /// v8（graph_runs 补 kind/detail_json）→ v9（discovery_leads 补拒因两列）。
    /// 各段均幂等（IF NOT EXISTS + 列探测），v6 源库多段连跑殊途同归；
    /// 纯增量、不动既有行——「删除重跑」政策对纯增量版本升格让位（数据零成本保全）。
    /// 任一 ALTER 失败即整体报错停火（Store::open 直接失败，半成品不落盘——
    /// WAL + 事务保证重跑从清洁起点再走）。
    fn migrate_to_current(&self) -> Result<()> {
        self.conn.execute_batch(MIGRATE_V6_TO_V7_SQL)?;
        for (table, column, alter) in [
            (
                "episodes",
                "lead_id",
                "ALTER TABLE episodes ADD COLUMN lead_id TEXT REFERENCES discovery_leads(dedupe_key)",
            ),
            (
                "graph_runs",
                "kind",
                "ALTER TABLE graph_runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'pipeline'",
            ),
            (
                "graph_runs",
                "detail_json",
                "ALTER TABLE graph_runs ADD COLUMN detail_json TEXT",
            ),
            // D9：v8→v9 段 = discovery_leads 拒绝留档两列（NULL=合法空态，不带 DEFAULT，
            // 与 SCHEMA_SQL/MIGRATE_V6_TO_V7_SQL 的 DDL 面完全一致）。
            (
                "discovery_leads",
                "reject_chips_json",
                "ALTER TABLE discovery_leads ADD COLUMN reject_chips_json TEXT",
            ),
            (
                "discovery_leads",
                "reject_note",
                "ALTER TABLE discovery_leads ADD COLUMN reject_note TEXT",
            ),
        ] {
            let exists: Option<i64> = self
                .conn
                .query_row(
                    &format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name=?"),
                    [column],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                self.conn.execute_batch(alter)?;
            }
        }
        self.conn.execute_batch(MIGRATE_V7_TO_V8_SQL)?;
        Ok(())
    }

    // ------------------------------------------------------------------ runs

    /// run 类型面（§8.6）：常规管线 run。graph_runs 历史只有这一个隐式类型。
    pub const RUN_KIND_PIPELINE: &str = "pipeline";
    /// run 类型面（§8.6 行 228/231）：entity_split / entity_merge 维护操作必记
    /// 一条 MAINTENANCE run（detail_json 载全参数，可回放审计）。
    pub const RUN_KIND_MAINTENANCE: &str = "maintenance";
    /// run 类型面（P0-4 复盘解耦）：collect 尾的复盘卡刷新 run——只重放房间语料
    /// Episode、零 AI 边。completed 照常记账，但 run_pair_delta 显式排除本类，
    /// 否则每日 collect 的 refresh 会把「vs 上轮感知」对照窗稀释成「无变化」。
    pub const RUN_KIND_RECAP_REFRESH: &str = "recap-refresh";
    /// run 类型面（R2 批 2 D1 WS 挂接）：WS 弹幕窗的场次窗 Episode 入账 run——
    /// 只入 WS 窗线、零 AI 边。`run_pair_delta` 显式排除本类（与 recap-refresh
    /// 同理：recording 相与 collect/episodes 同属**一个 collect run** 的面，
    /// 若把 ws-record 放进「vs 上轮感知」对照窗会稀释成「无变化」）。**不可从
    /// widgets 口子跑到**：仅由 collect 尾段挂接内部开出。
    pub const RUN_KIND_WS_RECORD: &str = "ws-record";

    pub fn begin_run(&self, model: &str) -> Result<String> {
        let run_id = format!("run:{}", uuid::Uuid::new_v4().simple());
        self.begin_run_fixed(&run_id, &self.now(), model)?;
        Ok(run_id)
    }

    /// 注入式 begin_run：黄金样本对账与回放测试用。
    pub fn begin_run_fixed(&self, run_id: &str, started_at: &str, model: &str) -> Result<()> {
        self.begin_run_typed(run_id, started_at, model, Self::RUN_KIND_PIPELINE, None)
    }

    /// 带类型与审计明细的 run 开账（v8 kind/detail_json 列的唯一写门）。
    pub fn begin_run_typed(
        &self,
        run_id: &str,
        started_at: &str,
        model: &str,
        kind: &str,
        detail_json: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO graph_runs(run_id, started_at, model, kind, detail_json) \
             VALUES(?,?,?,?,?)",
            params![run_id, started_at, model, kind, detail_json],
        )?;
        Ok(())
    }

    pub fn complete_run(&self, run_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE graph_runs SET completed_at=? WHERE run_id=? AND failed_at IS NULL",
            params![self.now(), run_id],
        )?;
        Ok(())
    }

    pub fn fail_run(&self, run_id: &str, error: &str, aborted: bool) -> Result<()> {
        let failure = json_canon(&serde_json::json!({
            "status": if aborted { "aborted" } else { "failed" },
            "error": error,
        }));
        self.conn.execute(
            "UPDATE graph_runs SET failed_at=?,failure_json=? \
             WHERE run_id=? AND completed_at IS NULL AND failed_at IS NULL",
            params![self.now(), failure, run_id],
        )?;
        Ok(())
    }
}
