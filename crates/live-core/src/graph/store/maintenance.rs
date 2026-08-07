//! §8.6 手动图维护：entity_split / entity_merge（store 受控写入族同族）。
//!
//! 体积备书：超 500 线 = split/merge 两大件共享 Store impl 内私有件
//! （canon_ids/savepoint/detail 账），同族隔离卷。缝 = 各成 split.rs/merge.rs
//! 需把私有 helper 上提到 store 根，收益不抵扩散——留同卷。
//!
//! 语义锚点（design 行 228-229 原文）：
//! - split：新建 entity，指定 mention 的归属重指到新实体（REFERS_TO 旧闭新开，
//!   与 upsert_mention 的归属切换同族区间语义）；证据落在这些 mention 上的关系/
//!   兴趣边（source_kind ∈ {ai_semantic, ai_state}）关闭区间（valid_to），其余边
//!   不动；操作写一条 kind='maintenance' 的 run，detail_json 载全参数可回放审计。
//! - merge：源实体的全部关联边（含已闭历史区间）与别名迁移到 target；活跃边按
//!   upsert 族查重键 (source,predicate,target,kind[,viewer]) 合流——evidence 并集
//!   去重保序、confidence=max、survivor 保留自己的 valid_from/first_seen；源实体
//!   关闭 = 行删除（entities 表无 valid_to 列，§8.6 的区间语义只给边定）。
//!
//! 幂等（行 229）：split 同参重放判据 = 确定性新实体 id 已存在 ∧ 指定 mention 的
//! 活跃归属已全部就位；merge 同参重放判据 = 源已全部缺失 ∧ 账上有同参 MAINTENANCE
//! 完成记录（缺账的「源从未存在」照 404 报错面，不与重放混）。参数 canon：滤空串
//! → 排序去重（输入序无关，写入与账单共用同一 canon）。
//!
//! 事实闸（§4 红线，merge/drop 同面）：merge 的源实体随并入被实删，故源必须全部
//! source_kind='ai'；平台事实实体只可作 target（被并入方，自身行不被改写）。
//! 本闸源于外部复审的如实指控：drop 有闸而 merge 漏闸，merge 路径可实删事实实体。

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};

use crate::episodes::{hash_parts, json_canon};

use super::{Store, StoreError, merge_props, with_savepoint};

/// MAINTENANCE run 的 model 位：手动操作无 AI 模型，字面占位区分来源。
pub const MAINTENANCE_RUN_MODEL: &str = "manual";

#[derive(Debug, thiserror::Error)]
pub enum MaintenanceError {
    /// 未知实体/mention（HTTP 404 面）。
    #[error("{0}")]
    NotFound(String),
    /// 参数形状或归属语义冲突（HTTP 422 面）。
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Store(#[from] StoreError),
}

impl From<rusqlite::Error> for MaintenanceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

type MResult<T> = std::result::Result<T, MaintenanceError>;

fn not_found<T>(message: impl Into<String>) -> MResult<T> {
    Err(MaintenanceError::NotFound(message.into()))
}

fn invalid<T>(message: impl Into<String>) -> MResult<T> {
    Err(MaintenanceError::Invalid(message.into()))
}

/// split 终态：run_id（重放时指回原始维护 run）、新实体 id、迁移/关闭计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOutcome {
    pub run_id: String,
    pub changed: bool,
    pub new_entity_id: String,
    pub moved_mentions: usize,
    pub closed_edges: usize,
}

/// merge 终态：run_id（重放时指回原始维护 run）、迁移/合流/别名计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    pub run_id: String,
    pub changed: bool,
    /// 坐标被重指的边行数（活跃 survivor + 已闭历史；被吸收折叠的不计入）。
    pub repointed_edges: usize,
    /// 活跃 quad 合流中被吸收关闭（valid_to）的边行数。
    pub folded_edges: usize,
    /// 迁移到目标的别名行数。
    pub merged_aliases: usize,
}

/// 参数 canon：滤空 → 排序去重（幂等键 = 集合而非序列）。
fn canon_ids(ids: &[String]) -> Vec<String> {
    let mut out: Vec<String> = ids
        .iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 确定性拆分目标 id：同参必同落点（重放判据与审计 detail 的身份锚）。
fn split_entity_id(entity_id: &str, entity_type: &str, mention_ids: &[String]) -> String {
    format!(
        "entity:{}:split-{}",
        crate::episodes::safe_type(entity_type),
        hash_parts(
            &[
                entity_id.to_string(),
                json_canon(&Value::Array(
                    mention_ids.iter().cloned().map(Value::String).collect(),
                )),
            ],
            18
        )
    )
}

impl Store {
    /// §8.6 `entity_split(entity_id, mention_ids[])`。
    pub fn entity_split(&self, entity_id: &str, mention_ids: &[String]) -> MResult<SplitOutcome> {
        let mentions = canon_ids(mention_ids);
        if mentions.is_empty() {
            return invalid("entity_split：mention_ids 不能为空");
        }
        let Some((name, entity_type, description)) = self.entity_brief(entity_id)? else {
            return not_found(format!("entity_split：实体 {entity_id} 不存在"));
        };
        for mention in &mentions {
            if !self.mention_exists(mention)? {
                return not_found(format!("entity_split：mention {mention} 不存在"));
            }
        }
        let new_entity_id = split_entity_id(entity_id, &entity_type, &mentions);
        let detail = split_detail(entity_id, &mentions, &new_entity_id);
        // 重放裁决（§8.6：同一 (entity_id, mention_ids) 重复执行不产生二次拆分）。
        let mut placed = self.entity_exists(&new_entity_id)?;
        if placed {
            for mention in &mentions {
                if self.active_refers_to(mention)?.as_deref() != Some(new_entity_id.as_str()) {
                    placed = false;
                    break;
                }
            }
        }
        if placed {
            return Ok(SplitOutcome {
                run_id: self.maintenance_run_of(&detail)?.unwrap_or_default(),
                changed: false,
                new_entity_id,
                moved_mentions: 0,
                closed_edges: 0,
            });
        }
        // §8.6：不属于该实体的 mention 显式报错（错文点名 mention 与实体）。
        for mention in &mentions {
            match self.active_refers_to(mention)? {
                Some(target) if target == entity_id => {}
                other => {
                    return invalid(format!(
                        "entity_split：mention {mention} 不属于实体 {entity_id}\
                         （当前归属：{}）",
                        other.as_deref().unwrap_or("<无活跃归属>")
                    ));
                }
            }
        }
        let detail_for_run = detail.clone();
        let split_to = new_entity_id.clone();
        with_savepoint("entity_split", self, move || {
            let now = self.now();
            let run_id = format!("run:{}", uuid::Uuid::new_v4().simple());
            self.begin_run_typed(
                &run_id,
                &now,
                MAINTENANCE_RUN_MODEL,
                Store::RUN_KIND_MAINTENANCE,
                Some(&detail_for_run),
            )?;
            let properties = json_canon(&serde_json::json!({
                "resolution": "MAINTENANCE_SPLIT",
                "split_from": entity_id,
            }));
            self.conn.execute(
                "INSERT INTO entities(\
                   entity_id,canonical_name,normalized_name,entity_type,description,source_kind,\
                   properties_json,first_seen_at,last_seen_at) \
                 VALUES(?,?,?,?,?,?,?,?,?)",
                params![
                    split_to,
                    name,
                    crate::episodes::norm(&name),
                    entity_type,
                    description,
                    "maintenance",
                    properties,
                    now,
                    now
                ],
            )?;
            // nodes 镜像：query.references 的 entities↔nodes 对齐断言面要求同生。
            self.upsert_node(
                &split_to,
                "Entity",
                &name,
                &serde_json::json!({
                    "entity_type": entity_type,
                    "description": description,
                }),
                "maintenance",
                Some(&now),
            )?;
            for mention in &mentions {
                let confidence: f64 = self.conn.query_row(
                    "SELECT confidence FROM mentions WHERE mention_id=?",
                    params![mention],
                    |row| row.get(0),
                )?;
                self.conn.execute(
                    "UPDATE edges SET valid_to=?,last_seen_at=? \
                     WHERE source_id=? AND predicate='REFERS_TO' AND source_kind='grounded_ai' \
                     AND valid_to IS NULL",
                    params![now, now, mention],
                )?;
                self.upsert_edge(
                    mention,
                    "REFERS_TO",
                    &split_to,
                    &serde_json::json!({"decision": "MAINTENANCE_SPLIT"}),
                    "grounded_ai",
                    Some(confidence),
                    std::slice::from_ref(mention),
                    &run_id,
                    None,
                )?;
            }
            let closed_edges = self.close_tainted_ai_edges(&mentions, &run_id, &now)?;
            self.complete_run(&run_id)?;
            Ok(SplitOutcome {
                run_id,
                changed: true,
                new_entity_id: split_to.clone(),
                moved_mentions: mentions.len(),
                closed_edges,
            })
        })
    }

    /// §8.6 `entity_merge(source_ids[], target_id)`（G2 提案通道的确定性应用端，
    /// G1 手动面先行：同一受控写入单元 + MAINTENANCE run 记）。
    pub fn entity_merge(&self, source_ids: &[String], target_id: &str) -> MResult<MergeOutcome> {
        let sources = canon_ids(source_ids);
        if sources.is_empty() {
            return invalid("entity_merge：source_ids 不能为空");
        }
        if sources.iter().any(|item| item == target_id) {
            return invalid("entity_merge：source 与 target 不得重叠");
        }
        if !self.entity_exists(target_id)? {
            return not_found(format!("entity_merge：目标实体 {target_id} 不存在"));
        }
        let mut missing: Vec<String> = Vec::new();
        for source in &sources {
            if !self.entity_exists(source)? {
                missing.push(source.clone());
            }
        }
        let detail = merge_detail(&sources, target_id);
        if !missing.is_empty() {
            // 幂等重放：全部缺失 ∧ 账上有同参完成记录 → 指回原 run；缺账 = 从未存在 → 404。
            if missing.len() == sources.len()
                && let Some(run_id) = self.maintenance_run_of(&detail)?
            {
                return Ok(MergeOutcome {
                    run_id,
                    changed: false,
                    repointed_edges: 0,
                    folded_edges: 0,
                    merged_aliases: 0,
                });
            }
            return not_found(format!("entity_merge：源实体不存在：{}", missing.join(",")));
        }
        // 事实闸（§4 红线，与 entity_drop 同面）：merge 会实删源实体——源必须全部是
        // AI 归属；平台事实 entity 只能做 target（被并入、自身行不被改写）。
        for source in &sources {
            let kind: String = self.conn.query_row(
                "SELECT source_kind FROM entities WHERE entity_id=?",
                params![source],
                |row| row.get(0),
            )?;
            if kind != "ai" {
                return invalid(format!(
                    "entity_merge：源实体 {source} 的 source_kind={kind}，\
                     仅 AI 归属（source_kind='ai'）实体可作 merge 源——平台事实只可当被并入方（target）"
                ));
            }
        }
        with_savepoint("entity_merge", self, move || {
            let now = self.now();
            let run_id = format!("run:{}", uuid::Uuid::new_v4().simple());
            self.begin_run_typed(
                &run_id,
                &now,
                MAINTENANCE_RUN_MODEL,
                Store::RUN_KIND_MAINTENANCE,
                Some(&detail),
            )?;
            let merged_aliases = self.migrate_aliases(&sources, target_id)?;
            let (repointed_edges, folded_edges) =
                self.merge_edges_onto(&sources, target_id, &run_id, &now)?;
            // 关闭（行删除）的 FK 序：源别名行 → 源 Entity 节点（edges→nodes 已迁毕）
            // → 源 entities 行（aliases→entities 已清）。
            let placeholders = vec!["?"; sources.len()].join(",");
            self.conn.execute(
                &format!("DELETE FROM entity_aliases WHERE entity_id IN ({placeholders})"),
                rusqlite::params_from_iter(sources.iter()),
            )?;
            for source in &sources {
                self.conn
                    .execute("DELETE FROM nodes WHERE node_id=?", params![source])?;
                self.conn
                    .execute("DELETE FROM entities WHERE entity_id=?", params![source])?;
            }
            self.complete_run(&run_id)?;
            Ok(MergeOutcome {
                run_id,
                changed: true,
                repointed_edges,
                folded_edges,
                merged_aliases,
            })
        })
    }

    /// `entity_drop(entity_id)`（实体归并：AI 裁决的整货删除面）。
    /// 关闭语义 = 行删除（entities 无 valid_to 列），范围：
    /// - 前置：实体必须存在；`source_kind` 必须为 `'ai'`（平台事实
    ///   bilibili_tag/bilibili_category/creator 等是 UI 事实面，读面依赖其存在，
    ///   非 AI 可删面——`'ai'` 之外的任何来源一律报错）；
    /// - 行删除：entity_aliases → nodes（Entity 镜像）→ entities；FK 序由内向外；
    /// - 引用释放：edges 任一端指向该实体的**整行删除**（edges 无 host 列，身份
    ///   即节点坐标；§8.6 区间语义只给 split/merge 边定义，drop 是收尸——不存在
    ///   可迁的宿主）；
    /// - mentions 无 entity 列（FK→episodes only，schema v8），结构性不触达；
    /// - 自身**不记账** run（design：入账归 reconcile 的外层维护 run——本操作是
    ///   事务原子单元，记账引出的嵌套 run 见 entity_merge 的同名注）。
    pub fn entity_drop(&self, entity_id: &str) -> MResult<()> {
        let source_kind: Option<String> = self
            .conn
            .query_row(
                "SELECT source_kind FROM entities WHERE entity_id=?",
                params![entity_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(source_kind) = source_kind else {
            return not_found(format!("entity_drop：实体 {entity_id} 不存在"));
        };
        if source_kind != "ai" {
            return invalid(format!(
                "entity_drop：实体 {entity_id} 的 source_kind={source_kind}，\
                 仅 AI 归属（source_kind='ai'）实体可删，平台事实/托管数据不可删"
            ));
        }
        with_savepoint("entity_drop", self, move || {
            self.conn.execute(
                "DELETE FROM edges WHERE source_id=? OR target_id=?",
                params![entity_id, entity_id],
            )?;
            self.conn.execute(
                "DELETE FROM entity_aliases WHERE entity_id=?",
                params![entity_id],
            )?;
            self.conn
                .execute("DELETE FROM nodes WHERE node_id=?", params![entity_id])?;
            self.conn
                .execute("DELETE FROM entities WHERE entity_id=?", params![entity_id])?;
            Ok(())
        })
    }

    // ------------------------------------------------------------- 共享取数面

    fn entity_brief(&self, entity_id: &str) -> MResult<Option<(String, String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT canonical_name,entity_type,description FROM entities WHERE entity_id=?",
                params![entity_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    fn mention_exists(&self, mention_id: &str) -> MResult<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM mentions WHERE mention_id=?",
                params![mention_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// mention 的活跃 REFERS_TO target（grounded_ai / valid_to IS NULL）。
    fn active_refers_to(&self, mention_id: &str) -> MResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT target_id FROM edges WHERE source_id=? AND predicate='REFERS_TO' \
                 AND source_kind='grounded_ai' AND valid_to IS NULL",
                params![mention_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// 同参 MAINTENANCE 完成记录的 run_id（重放指回的依据；多记录取最近一条）。
    fn maintenance_run_of(&self, detail_json: &str) -> MResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT run_id FROM graph_runs WHERE kind=? AND completed_at IS NOT NULL \
                 AND detail_json=? ORDER BY started_at DESC, run_id DESC LIMIT 1",
                params![Store::RUN_KIND_MAINTENANCE, detail_json],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// split 的关闭面（§8.6「证据落在这些 mention 上的关系/兴趣边关闭区间」）：
    /// 关系/兴趣边 = ai_semantic ∪ ai_state 的活跃行；REFERS_TO 走归属重指不属此面，
    /// CONTAINS_MENTION 是事实层不可动。evidence 交集判定在 Rust 侧做（确定性序）。
    fn close_tainted_ai_edges(
        &self,
        mention_ids: &[String],
        run_id: &str,
        now: &str,
    ) -> MResult<usize> {
        let wanted: std::collections::HashSet<&str> =
            mention_ids.iter().map(String::as_str).collect();
        let mut stmt = self.conn.prepare(
            "SELECT edge_id,evidence_json FROM edges WHERE valid_to IS NULL \
             AND source_kind IN ('ai_semantic','ai_state') ORDER BY edge_id",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut closed = 0;
        for (edge_id, evidence_json) in rows {
            let evidence: Vec<String> = serde_json::from_str(&evidence_json).unwrap_or_default();
            if evidence.iter().any(|item| wanted.contains(item.as_str())) {
                self.close_edge(&edge_id, run_id, now)?;
                closed += 1;
            }
        }
        Ok(closed)
    }

    /// 别名迁移：源实体行迁至 target；同 (alias_key,entity_id) 冲突与 upsert_alias
    /// 同族——confidence=max，alias 正文刷为 excluded（新值覆盖）。
    fn migrate_aliases(&self, sources: &[String], target_id: &str) -> MResult<usize> {
        let placeholders = vec!["?"; sources.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT alias_key,alias,source_kind,confidence,created_at FROM entity_aliases \
             WHERE entity_id IN ({placeholders}) ORDER BY entity_id,alias_key"
        ))?;
        let rows: Vec<(String, String, String, f64, String)> = stmt
            .query_map(rusqlite::params_from_iter(sources.iter()), |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let migrated = rows.len();
        for (alias_key, alias, source_kind, confidence, created_at) in rows {
            self.conn.execute(
                "INSERT INTO entity_aliases(alias_key,entity_id,alias,source_kind,confidence,created_at) \
                 VALUES(?,?,?,?,?,?) \
                 ON CONFLICT(alias_key,entity_id) DO UPDATE SET \
                   alias=excluded.alias,confidence=MAX(entity_aliases.confidence,excluded.confidence)",
                params![alias_key, target_id, alias, source_kind, confidence, created_at],
            )?;
        }
        Ok(migrated)
    }

    /// 边迁移与合流。候选域 = 任一端落在 sources ∪ {target} 的边（合流后可能
    /// 撞 quad 的全集）。已闭行只重指坐标；活跃行按 upsert 查重键分组合流：
    /// survivor = (first_seen_at, edge_id) 全序最小（first_seen 不变=最老存活），
    /// props 逐被吸收行覆盖合并、evidence 并集去重保序、confidence=max。
    fn merge_edges_onto(
        &self,
        sources: &[String],
        target_id: &str,
        run_id: &str,
        now: &str,
    ) -> MResult<(usize, usize)> {
        struct EdgeRow {
            edge_id: String,
            source_id: String,
            target_id: String,
            predicate: String,
            source_kind: String,
            properties_json: String,
            confidence: Option<f64>,
            evidence_json: String,
            closed: bool,
        }
        let mut domain: Vec<String> = sources.to_vec();
        domain.push(target_id.to_string());
        let placeholders = vec!["?"; domain.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT edge_id,source_id,target_id,predicate,source_kind,properties_json,\
                    confidence,evidence_json,valid_to \
             FROM edges WHERE source_id IN ({placeholders}) OR target_id IN ({placeholders}) \
             ORDER BY first_seen_at, edge_id"
        ))?;
        let rows: Vec<EdgeRow> = stmt
            .query_map(
                rusqlite::params_from_iter(domain.iter().chain(domain.iter())),
                |row| {
                    Ok(EdgeRow {
                        edge_id: row.get(0)?,
                        source_id: row.get(1)?,
                        target_id: row.get(2)?,
                        predicate: row.get(3)?,
                        source_kind: row.get(4)?,
                        properties_json: row.get(5)?,
                        confidence: row.get(6)?,
                        evidence_json: row.get(7)?,
                        closed: row.get::<_, Option<String>>(8)?.is_some(),
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let remap = |node: &str| {
            if sources.iter().any(|source| source == node) {
                target_id.to_string()
            } else {
                node.to_string()
            }
        };
        let mut repointed = 0usize;
        let mut folded = 0usize;
        let mut groups: std::collections::BTreeMap<
            (String, String, String, String, String),
            Vec<EdgeRow>,
        > = std::collections::BTreeMap::new();
        for row in rows {
            let final_source = remap(&row.source_id);
            let final_target = remap(&row.target_id);
            let changed = final_source != row.source_id || final_target != row.target_id;
            if row.closed {
                if changed {
                    // 历史区间只重指坐标：valid_* 与 last_seen 冻结（审计痕）。
                    self.conn.execute(
                        "UPDATE edges SET source_id=?,target_id=? WHERE edge_id=?",
                        params![final_source, final_target, row.edge_id],
                    )?;
                    repointed += 1;
                }
                continue;
            }
            // 查重键与 upsert_edge 同构：ai_semantic 加 owner（properties.viewer_id）位。
            let owner = if row.source_kind == "ai_semantic" {
                serde_json::from_str::<Value>(&row.properties_json)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("viewer_id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            groups
                .entry((
                    final_source,
                    row.predicate.clone(),
                    final_target,
                    row.source_kind.clone(),
                    owner,
                ))
                .or_default()
                .push(row);
        }
        for group in groups.into_values() {
            let (survivor, absorbed) = group.split_first().expect("组非空（BTreeMap entry API）");
            let survivor_final_source = remap(&survivor.source_id);
            let survivor_final_target = remap(&survivor.target_id);
            let coords_changed = survivor_final_source != survivor.source_id
                || survivor_final_target != survivor.target_id;
            let mut props: Map<String, Value> =
                serde_json::from_str(&survivor.properties_json).unwrap_or_default();
            let mut evidence: Vec<String> =
                serde_json::from_str(&survivor.evidence_json).unwrap_or_default();
            let mut confidence = survivor.confidence;
            for member in absorbed {
                let member_props: Map<String, Value> =
                    serde_json::from_str(&member.properties_json).unwrap_or_default();
                props = merge_props(member_props, &json_canon(&Value::Object(props)));
                let mut member_evidence: Vec<String> =
                    serde_json::from_str(&member.evidence_json).unwrap_or_default();
                evidence.append(&mut member_evidence);
                confidence = match (confidence, member.confidence) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (Some(a), None) => Some(a),
                    (None, b) => b,
                };
                self.conn.execute(
                    "UPDATE edges SET valid_to=?,last_seen_at=? WHERE edge_id=?",
                    params![now, now, member.edge_id],
                )?;
                folded += 1;
            }
            // 去重保序（Python dict.fromkeys——upsert_edge 同族口径）。
            let mut deduped: Vec<String> = Vec::new();
            for item in evidence {
                if !deduped.contains(&item) {
                    deduped.push(item);
                }
            }
            if coords_changed || !absorbed.is_empty() {
                self.conn.execute(
                    "UPDATE edges SET source_id=?,target_id=?,properties_json=?,confidence=?,\
                     evidence_json=?,last_seen_at=?,run_id=? WHERE edge_id=?",
                    params![
                        survivor_final_source,
                        survivor_final_target,
                        json_canon(&Value::Object(props)),
                        confidence,
                        json_canon(&Value::Array(
                            deduped.iter().cloned().map(Value::String).collect()
                        )),
                        now,
                        run_id,
                        survivor.edge_id,
                    ],
                )?;
            }
            if coords_changed {
                repointed += 1;
            }
        }
        Ok((repointed, folded))
    }
}

fn split_detail(entity_id: &str, mention_ids: &[String], new_entity_id: &str) -> String {
    json_canon(&serde_json::json!({
        "op": "entity_split",
        "entity_id": entity_id,
        "mention_ids": mention_ids,
        "new_entity_id": new_entity_id,
    }))
}

fn merge_detail(source_ids: &[String], target_id: &str) -> String {
    json_canon(&serde_json::json!({
        "op": "entity_merge",
        "source_ids": source_ids,
        "target_id": target_id,
    }))
}
