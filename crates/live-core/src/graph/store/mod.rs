//! 时序属性图存储（移植 Python `graph.py` 的 GraphRepository）。
//!
//! schema v6（设计文档 §8.1）相对 v5 的**显式升级**：
//! 1. 真实外键（edges→nodes、edges.run_id→graph_runs、mentions→episodes、
//!    entity_aliases→entities）；
//! 2. edges 增加 `viewer_id` 列（close_missing_viewer_semantic_edges 的行为判定键，
//!    等价 v5 的 json_extract；properties_json 原样保留，仓库字节兼容）；
//! 3. TARGETS/ABOUT 等 action 边必带 confidence（见 build.rs）；
//! 4. user_version = 6，旧库沿用“删除重跑”政策，不做迁移。
//!
//! 幂等语义与 v5 一致：节点属性合并保鲜、活跃边查重-合并、evidence 合并且去重保序、
//! confidence 取 max、first_seen 不变。
//!
//! 文件拆分：本文件 = schema v6 / 连接与事务 / 运行段 / 共享辅助；
//! nodes.rs / edges.rs / entities.rs / mentions.rs 各自一类受控写入。

mod edges;
mod entities;
mod mentions;
mod nodes;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use crate::episodes::json_canon;

pub use mentions::mention_id_of;

pub const GRAPH_SCHEMA_VERSION: i64 = 6;
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
    model TEXT
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
    last_seen_at TEXT NOT NULL
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
                return repo_err(format!(
                    "outdated graph database; delete the store and rerun (user_version={version})"
                ));
            }
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn
            .pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)?;
        Ok(())
    }

    // ------------------------------------------------------------------ runs

    pub fn begin_run(&self, model: &str) -> Result<String> {
        let run_id = format!("run:{}", uuid::Uuid::new_v4().simple());
        self.begin_run_fixed(&run_id, &self.now(), model)?;
        Ok(run_id)
    }

    /// 注入式 begin_run：黄金样本对账与回放测试用。
    pub fn begin_run_fixed(&self, run_id: &str, started_at: &str, model: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO graph_runs(run_id, started_at, model) VALUES(?,?,?)",
            params![run_id, started_at, model],
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
