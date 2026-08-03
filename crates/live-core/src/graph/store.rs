//! 时序属性图存储（移植 Python `graph.py` 的 GraphRepository）。
//!
//! schema v6（设计文档 §8.1）相对 v5 的**显式升级**：
//! 1. 真实外键（edges→nodes、edges.run_id→graph_runs、mentions→episodes、
//!    entity_aliases→entities）；
//! 2. edges 增加 `viewer_id` 列（从 properties JSON 提升，纯索引用途；
//!    properties_json 原样保留，保证仓库字节兼容）；
//! 3. TARGETS/ABOUT 等 action 边必带 confidence（见 build.rs）；
//! 4. user_version=6，旧库沿用"删除重跑"政策，不做迁移。
//!
//! 幂等语义与 v5 一致：节点属性合并保鲜、活跃边查重-合并、evidence 合并且去重保序、
//! confidence 取 max、first_seen 不变。

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use crate::episodes::{Episode, hash_parts, json_canon, norm, py_repr_list, safe_type};
use crate::models::EntityProposal;

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
    pub conn: Connection,
    clock: Box<dyn Fn() -> String + Send + Sync>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_clock(path, Box::new(crate::episodes::now_iso))
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::open_with_clock(Path::new(":memory:"), Box::new(crate::episodes::now_iso))
    }

    pub fn open_with_clock(
        path: &Path,
        clock: Box<dyn Fn() -> String + Send + Sync>,
    ) -> Result<Self> {
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

    // ----------------------------------------------------------------- nodes

    pub fn upsert_node(
        &self,
        node_id: &str,
        node_type: &str,
        name: &str,
        properties: &Value,
        source_kind: &str,
        seen_at: Option<&str>,
    ) -> Result<()> {
        let now = seen_at.map(str::to_string).unwrap_or_else(|| self.now());
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT properties_json,first_seen_at FROM nodes WHERE node_id=?",
                params![node_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let mut merged = properties.as_object().cloned().unwrap_or_default();
        let mut first_seen = now.clone();
        if let Some((props_json, seen)) = row {
            first_seen = seen;
            // Python old.update(new)：新值覆盖同键。
            if let Value::Object(old) =
                serde_json::from_str::<Value>(&props_json).unwrap_or(Value::Null)
            {
                let mut union = old;
                for (key, value) in merged {
                    union.insert(key, value);
                }
                merged = union;
            }
        }
        self.conn.execute(
            "INSERT INTO nodes(node_id,node_type,name,properties_json,source_kind,first_seen_at,last_seen_at) \
             VALUES(?,?,?,?,?,?,?) \
             ON CONFLICT(node_id) DO UPDATE SET \
               node_type=excluded.node_type, name=excluded.name, \
               properties_json=excluded.properties_json, source_kind=excluded.source_kind, \
               last_seen_at=excluded.last_seen_at",
            params![
                node_id,
                node_type,
                name,
                json_canon(&Value::Object(merged)),
                source_kind,
                first_seen,
                now
            ],
        )?;
        Ok(())
    }

    pub fn upsert_platform_entity(
        &self,
        entity_id: &str,
        canonical_name: &str,
        entity_type: &str,
        properties: &Value,
    ) -> Result<()> {
        let now = self.now();
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT first_seen_at,properties_json FROM entities WHERE entity_id=?",
                params![entity_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let first_seen = row
            .as_ref()
            .map(|(seen, _)| seen.clone())
            .unwrap_or_else(|| now.clone());
        let mut merged = properties.as_object().cloned().unwrap_or_default();
        merged.insert(
            "identity_source".to_string(),
            Value::String("bilibili".to_string()),
        );
        if let Some((_, props_json)) = row
            && let Value::Object(old) =
                serde_json::from_str::<Value>(&props_json).unwrap_or(Value::Null)
        {
            let mut union = old;
            for (key, value) in merged {
                union.insert(key, value);
            }
            merged = union;
        }
        let merged_value = Value::Object(merged);
        self.conn.execute(
            "INSERT INTO entities(\
               entity_id,canonical_name,normalized_name,entity_type,description,source_kind,\
               properties_json,first_seen_at,last_seen_at) \
             VALUES(?,?,?,?,?,?,?,?,?) \
             ON CONFLICT(entity_id) DO UPDATE SET \
               canonical_name=excluded.canonical_name, normalized_name=excluded.normalized_name, \
               entity_type=excluded.entity_type, source_kind=excluded.source_kind, \
               properties_json=excluded.properties_json, last_seen_at=excluded.last_seen_at",
            params![
                entity_id,
                canonical_name,
                norm(canonical_name),
                entity_type,
                "",
                "platform_fact",
                json_canon(&merged_value),
                first_seen,
                now
            ],
        )?;
        // 节点 properties：{"entity_type": et, **merged}
        let mut node_props = Map::new();
        node_props.insert(
            "entity_type".to_string(),
            Value::String(entity_type.to_string()),
        );
        if let Value::Object(map) = &merged_value {
            for (key, value) in map {
                node_props.insert(key.clone(), value.clone());
            }
        }
        self.upsert_node(
            entity_id,
            "Entity",
            canonical_name,
            &Value::Object(node_props),
            "platform_fact",
            Some(&now.clone()),
        )?;
        self.conn.execute(
            "INSERT INTO entity_aliases(alias_key,entity_id,alias,source_kind,confidence,created_at) \
             VALUES(?,?,?,?,?,?) \
             ON CONFLICT(alias_key,entity_id) DO UPDATE SET alias=excluded.alias,confidence=1.0",
            params![norm(canonical_name), entity_id, canonical_name, "platform_fact", 1.0, now],
        )?;
        Ok(())
    }

    // ----------------------------------------------------------------- edges

    /// 活跃边查重-合并。返回最终 edge_id。
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_edge(
        &self,
        source_id: &str,
        predicate: &str,
        target_id: &str,
        properties: &Value,
        source_kind: &str,
        confidence: Option<f64>,
        evidence_ids: &[String],
        run_id: &str,
        seen_at: Option<&str>,
    ) -> Result<String> {
        let now = seen_at.map(str::to_string).unwrap_or_else(|| self.now());
        let mut merged_props = properties.as_object().cloned().unwrap_or_default();
        let owner_id = if source_kind == "ai_semantic" {
            merged_props
                .get("viewer_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        let row: Option<(String, String, String, String, Option<f64>)> = if owner_id.is_empty() {
            self.conn
                .query_row(
                    "SELECT edge_id,first_seen_at,properties_json,evidence_json,confidence \
                     FROM edges WHERE source_id=? AND predicate=? AND target_id=? AND source_kind=? \
                     AND valid_to IS NULL ORDER BY valid_from DESC LIMIT 1",
                    params![source_id, predicate, target_id, source_kind],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
        } else {
            self.conn
                .query_row(
                    "SELECT edge_id,first_seen_at,properties_json,evidence_json,confidence \
                     FROM edges WHERE source_id=? AND predicate=? AND target_id=? AND source_kind=? \
                     AND valid_to IS NULL AND viewer_id=? ORDER BY valid_from DESC LIMIT 1",
                    params![source_id, predicate, target_id, source_kind, owner_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
        };
        let mut merged_evidence: Vec<String> = evidence_ids
            .iter()
            .filter(|item| !item.is_empty())
            .cloned()
            .collect();
        let mut merged_confidence = confidence;
        if let Some((_, _, props_json, evidence_json, old_conf)) = &row {
            if let Value::Object(old) =
                serde_json::from_str::<Value>(props_json).unwrap_or(Value::Null)
            {
                let mut union = old;
                for (key, value) in merged_props {
                    union.insert(key, value);
                }
                merged_props = union;
            }
            let mut old_evidence: Vec<String> =
                serde_json::from_str(evidence_json).unwrap_or_default();
            old_evidence.extend(merged_evidence);
            merged_evidence = old_evidence;
            merged_confidence = match (old_conf, merged_confidence) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(*a),
                (None, b) => b,
            };
        }
        // 去重保序（Python dict.fromkeys）
        let mut deduped: Vec<String> = Vec::new();
        for item in merged_evidence {
            if !deduped.contains(&item) {
                deduped.push(item);
            }
        }
        merged_evidence = deduped;
        if let Some((edge_id, _, _, _, _)) = row {
            self.conn.execute(
                "UPDATE edges SET properties_json=?,confidence=?,evidence_json=?,last_seen_at=?,run_id=? \
                 WHERE edge_id=?",
                params![
                    json_canon(&Value::Object(merged_props)),
                    merged_confidence,
                    json_canon(&Value::Array(
                        merged_evidence.iter().cloned().map(Value::String).collect()
                    )),
                    now,
                    run_id,
                    edge_id
                ],
            )?;
            return Ok(edge_id);
        }
        let edge_id = format!(
            "edge:{}",
            hash_parts(
                &[
                    source_id.to_string(),
                    predicate.to_string(),
                    target_id.to_string(),
                    source_kind.to_string(),
                    owner_id.clone(),
                    now.clone(),
                    run_id.to_string(),
                ],
                24,
            )
        );
        // viewer_id 列：ai_semantic 取 owner，否则 viewer: 前缀派生（纯索引用途）。
        let viewer_column = if !owner_id.is_empty() {
            owner_id
        } else {
            source_id
                .strip_prefix("viewer:")
                .map(str::to_string)
                .unwrap_or_default()
        };
        self.conn.execute(
            "INSERT INTO edges(\
               edge_id,source_id,predicate,target_id,properties_json,source_kind,confidence,\
               evidence_json,valid_from,valid_to,first_seen_at,last_seen_at,run_id,viewer_id) \
             VALUES(?,?,?,?,?,?,?,?,?,NULL,?,?,?,?)",
            params![
                edge_id,
                source_id,
                predicate,
                target_id,
                json_canon(&Value::Object(merged_props)),
                source_kind,
                merged_confidence,
                json_canon(&Value::Array(
                    merged_evidence.iter().cloned().map(Value::String).collect()
                )),
                now,
                now,
                now,
                run_id,
                viewer_column
            ],
        )?;
        Ok(edge_id)
    }

    pub fn close_active_edges(
        &self,
        source_id: &str,
        predicate: &str,
        source_kind: &str,
        run_id: &str,
    ) -> Result<()> {
        let now = self.now();
        self.conn.execute(
            "UPDATE edges SET valid_to=?,last_seen_at=? \
             WHERE source_id=? AND predicate=? AND source_kind=? AND valid_to IS NULL AND run_id<>?",
            params![now, now, source_id, predicate, source_kind, run_id],
        )?;
        Ok(())
    }

    pub fn close_missing_viewer_semantic_edges(&self, viewer_id: &str, run_id: &str) -> Result<()> {
        let now = self.now();
        self.conn.execute(
            "UPDATE edges SET valid_to=?,last_seen_at=? \
             WHERE source_kind='ai_semantic' AND valid_to IS NULL \
               AND viewer_id=? AND (run_id IS NULL OR run_id<>?)",
            params![now, now, viewer_id, run_id],
        )?;
        Ok(())
    }

    pub fn active_edges(
        &self,
        source_id: &str,
        predicate: &str,
        source_kind: &str,
    ) -> Result<Vec<ActiveEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT edge_id,target_id,properties_json FROM edges \
             WHERE source_id=? AND predicate=? AND source_kind=? AND valid_to IS NULL \
             ORDER BY valid_from DESC",
        )?;
        let rows = stmt
            .query_map(params![source_id, predicate, source_kind], |row| {
                Ok(ActiveEdge {
                    edge_id: row.get(0)?,
                    target_id: row.get(1)?,
                    properties_json: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn close_edge(&self, edge_id: &str, run_id: &str, seen_at: &str) -> Result<()> {
        let _ = run_id; // run_id 仅记录审计意图，列上与 Python 行为一致只更新 valid_to/last_seen
        self.conn.execute(
            "UPDATE edges SET valid_to=?,last_seen_at=? WHERE edge_id=? AND valid_to IS NULL",
            params![seen_at, seen_at, edge_id],
        )?;
        Ok(())
    }

    // --------------------------------------------------------------- episode

    pub fn upsert_episode(&self, episode: &Episode) -> Result<()> {
        let now = self.now();
        let fields_value = Value::Array(
            episode
                .fields
                .iter()
                .map(|field| {
                    let mut item = Map::new();
                    item.insert("path".to_string(), Value::String(field.path.clone()));
                    item.insert("text".to_string(), Value::String(field.text.clone()));
                    item.insert("kind".to_string(), Value::String(field.kind.clone()));
                    Value::Object(item)
                })
                .collect(),
        );
        let content_hash = hash_parts(
            &[json_canon(&serde_json::json!({
                "viewer_id": episode.viewer_id,
                "source": episode.source,
                "event_type": episode.event_type,
                "published_at": episode.published_at,
                "title": episode.title,
                "url": episode.url,
                "bvid": episode.bvid,
                "fields": fields_value,
                "platform_facts": episode.platform_facts,
            }))],
            40,
        );
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT content_hash FROM episodes WHERE episode_id=?",
                params![episode.episode_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(old_hash) = existing {
            if old_hash != content_hash {
                return repo_err(format!(
                    "immutable Episode conflict: {}",
                    episode.episode_id
                ));
            }
            self.conn.execute(
                "UPDATE episodes SET last_seen_at=? WHERE episode_id=?",
                params![now, episode.episode_id],
            )?;
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO episodes(\
               episode_id,viewer_id,source,event_type,observed_at,published_at,title,url,bvid,\
               fields_json,platform_facts_json,content_hash,first_seen_at,last_seen_at) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                episode.episode_id,
                episode.viewer_id,
                episode.source,
                episode.event_type,
                episode.observed_at,
                episode.published_at,
                episode.title,
                episode.url,
                episode.bvid,
                json_canon(&fields_value),
                json_canon(&episode.platform_facts),
                content_hash,
                now,
                now
            ],
        )?;
        Ok(())
    }

    // --------------------------------------------------------------- entity

    pub fn entity_exists(&self, candidate_id: &str) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM entities WHERE entity_id=?",
                params![candidate_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// 解析实体提案：返回 (resolved_entity_id 或 "", decision)。
    pub fn resolve_entity(
        &self,
        proposal: &EntityProposal,
        _run_id: &str,
        viewer_id: &str,
        evidence_mention_ids: &[String],
    ) -> Result<(String, String)> {
        let name = proposal.canonical_name.trim().to_string();
        let entity_type = {
            let raw = proposal.entity_type.trim();
            if raw.is_empty() {
                "concept".to_string()
            } else {
                raw.to_string()
            }
        };
        let aliases: Vec<String> = proposal
            .aliases
            .iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        let existing = proposal
            .existing_entity_id
            .clone()
            .unwrap_or_default()
            .trim()
            .to_string();
        let decision = if proposal.resolution.is_empty() {
            "NEW_ENTITY".to_string()
        } else {
            proposal.resolution.clone()
        };
        if decision == "SAME_AS" {
            if existing.is_empty() || !self.entity_exists(&existing)? {
                let shown = if existing.is_empty() {
                    "<empty>"
                } else {
                    existing.as_str()
                };
                return repo_err(format!("SAME_AS references unknown entity: {shown}"));
            }
            let now = self.now();
            for alias in dedup_keep_order(std::iter::once(name.clone()).chain(aliases.clone())) {
                self.conn.execute(
                    "INSERT INTO entity_aliases(alias_key,entity_id,alias,source_kind,confidence,created_at) \
                     VALUES(?,?,?,?,?,?) \
                     ON CONFLICT(alias_key,entity_id) DO UPDATE SET \
                       alias=excluded.alias,confidence=MAX(entity_aliases.confidence,excluded.confidence)",
                    params![norm(&alias), existing, alias, "ai", proposal.confidence, now],
                )?;
            }
            return Ok((existing, decision));
        }
        if decision == "UNCERTAIN" {
            return Ok((String::new(), decision));
        }
        if decision != "NEW_ENTITY" {
            return repo_err(format!("unknown entity resolution decision: {decision}"));
        }

        let mut grounding: Vec<String> = evidence_mention_ids
            .iter()
            .filter(|item| !item.is_empty())
            .cloned()
            .collect();
        grounding.sort();
        grounding.dedup();
        let tie_break = if grounding.is_empty() {
            let sorted_aliases = {
                let mut copy = aliases.clone();
                copy.sort();
                copy
            };
            json_canon(&serde_json::json!([
                name,
                sorted_aliases,
                proposal.description
            ]))
        } else {
            String::new()
        };
        let resolved = format!(
            "entity:{}:{}",
            safe_type(&entity_type),
            hash_parts(
                &[
                    viewer_id.to_string(),
                    entity_type.clone(),
                    py_repr_list(&grounding),
                    tie_break,
                ],
                18,
            )
        );
        let now = self.now();
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT first_seen_at,properties_json FROM entities WHERE entity_id=?",
                params![resolved],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let first_seen = row
            .as_ref()
            .map(|(seen, _)| seen.clone())
            .unwrap_or_else(|| now.clone());
        let mut properties = Map::new();
        properties.insert("resolution".to_string(), Value::String(decision.clone()));
        properties.insert(
            "confidence".to_string(),
            serde_json::Number::from_f64(proposal.confidence)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        if let Some((_, props_json)) = row
            && let Value::Object(old) =
                serde_json::from_str::<Value>(&props_json).unwrap_or(Value::Null)
        {
            let mut union = old;
            for (key, value) in properties {
                union.insert(key, value);
            }
            properties = union;
        }
        self.conn.execute(
            "INSERT INTO entities(\
               entity_id,canonical_name,normalized_name,entity_type,description,source_kind,\
               properties_json,first_seen_at,last_seen_at) \
             VALUES(?,?,?,?,?,?,?,?,?) \
             ON CONFLICT(entity_id) DO UPDATE SET \
               canonical_name=excluded.canonical_name, \
               description=CASE WHEN excluded.description<>'' THEN excluded.description ELSE entities.description END, \
               properties_json=excluded.properties_json, last_seen_at=excluded.last_seen_at",
            params![
                resolved,
                name,
                norm(&name),
                entity_type,
                proposal.description,
                "ai",
                json_canon(&Value::Object(properties)),
                first_seen,
                now
            ],
        )?;
        let mut node_props = Map::new();
        node_props.insert("entity_type".to_string(), Value::String(entity_type));
        node_props.insert(
            "description".to_string(),
            Value::String(proposal.description.clone()),
        );
        self.upsert_node(
            &resolved,
            "Entity",
            &name,
            &Value::Object(node_props),
            "ai",
            None,
        )?;
        for alias in dedup_keep_order(std::iter::once(name.clone()).chain(aliases.clone())) {
            self.conn.execute(
                "INSERT INTO entity_aliases(alias_key,entity_id,alias,source_kind,confidence,created_at) \
                 VALUES(?,?,?,?,?,?) \
                 ON CONFLICT(alias_key,entity_id) DO UPDATE SET \
                   alias=excluded.alias,confidence=MAX(entity_aliases.confidence,excluded.confidence)",
                params![norm(&alias), resolved, alias, "ai", proposal.confidence, now],
            )?;
        }
        Ok((resolved, decision))
    }

    // -------------------------------------------------------------- mentions

    pub fn upsert_mention(
        &self,
        mention: &crate::models::MentionSpan,
        viewer_id: &str,
        run_id: &str,
        resolved_entity_id: Option<&str>,
        decision: &str,
    ) -> Result<String> {
        let mention_id = mention_id_of(viewer_id, mention);
        let now = self.now();
        self.conn.execute(
            "INSERT INTO mentions(\
               mention_id,episode_id,viewer_id,field_path,text,start_offset,end_offset,mention_type,\
               origin,proposed_entity_name,proposed_entity_type,confidence,run_id,created_at) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) \
             ON CONFLICT(mention_id) DO UPDATE SET confidence=excluded.confidence,run_id=excluded.run_id",
            params![
                mention_id,
                mention.episode_id,
                viewer_id,
                mention.field_path,
                mention.text,
                mention.start,
                mention.end,
                mention.mention_type,
                if mention.origin.is_empty() {
                    "explicit"
                } else {
                    mention.origin.as_str()
                },
                mention.proposed_entity_name,
                mention.proposed_entity_type,
                mention.confidence,
                run_id,
                now
            ],
        )?;
        let mut node_props = Map::new();
        node_props.insert(
            "episode_id".to_string(),
            Value::String(mention.episode_id.clone()),
        );
        node_props.insert(
            "field_path".to_string(),
            Value::String(mention.field_path.clone()),
        );
        node_props.insert("start".to_string(), Value::from(mention.start));
        node_props.insert("end".to_string(), Value::from(mention.end));
        node_props.insert(
            "origin".to_string(),
            if mention.origin.is_empty() {
                Value::Null
            } else {
                Value::String(mention.origin.clone())
            },
        );
        self.upsert_node(
            &mention_id,
            "Mention",
            &mention.text,
            &Value::Object(node_props),
            "grounded_ai",
            None,
        )?;
        self.upsert_edge(
            &mention.episode_id,
            "CONTAINS_MENTION",
            &mention_id,
            &serde_json::json!({}),
            "grounded_ai",
            Some(mention.confidence),
            std::slice::from_ref(&mention_id),
            run_id,
            None,
        )?;
        if let Some(resolved) = resolved_entity_id {
            self.conn.execute(
                "UPDATE edges SET valid_to=?,last_seen_at=? \
                 WHERE source_id=? AND predicate='REFERS_TO' AND source_kind='grounded_ai' \
                 AND target_id<>? AND valid_to IS NULL",
                params![now, now, mention_id, resolved],
            )?;
            self.upsert_edge(
                &mention_id,
                "REFERS_TO",
                resolved,
                &serde_json::json!({"decision": decision}),
                "grounded_ai",
                Some(mention.confidence),
                std::slice::from_ref(&mention_id),
                run_id,
                None,
            )?;
        } else {
            self.conn.execute(
                "UPDATE edges SET valid_to=?,last_seen_at=? \
                 WHERE source_id=? AND predicate='REFERS_TO' AND source_kind='grounded_ai' \
                 AND valid_to IS NULL",
                params![now, now, mention_id],
            )?;
        }
        Ok(mention_id)
    }
}

/// `mention:{viewer}:{hash24(episode_id, field_path, start, end, text)}`
/// （start/end 走 Python int-or-""：0 → ""）。
pub fn mention_id_of(viewer_id: &str, mention: &crate::models::MentionSpan) -> String {
    format!(
        "mention:{}:{}",
        viewer_id,
        hash_parts(
            &[
                mention.episode_id.clone(),
                mention.field_path.clone(),
                crate::episodes::py_str_int(mention.start),
                crate::episodes::py_str_int(mention.end),
                mention.text.clone(),
            ],
            24,
        )
    )
}

fn dedup_keep_order(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for item in items {
        if !result.contains(&item) {
            result.push(item);
        }
    }
    result
}
