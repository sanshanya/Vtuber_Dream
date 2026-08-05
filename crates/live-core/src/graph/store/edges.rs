//! 边：活跃查重-合并、证据合并并去重、时间区间关闭。

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::episodes::{hash_parts, json_canon};

use super::{ActiveEdge, Result, Store, StoreError, merge_props};

impl Store {
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
        // 轮2-R1-B2：双分支归一（owner 谓词 = `?='' OR viewer_id=?`，空 owner 即不 scoped
        // 与修前「无 viewer_id 条件」严格同义）；valid_from 平局 tie-break 追加 rowid DESC
        // ——取物理最新插入行，修前平局计划依赖，新证据可能落进旧边。
        let row: Option<(String, String, String, String, Option<f64>)> = self
            .conn
            .query_row(
                "SELECT edge_id,first_seen_at,properties_json,evidence_json,confidence \
                 FROM edges WHERE source_id=? AND predicate=? AND target_id=? AND source_kind=? \
                 AND valid_to IS NULL AND (?='' OR viewer_id=?) \
                 ORDER BY valid_from DESC, rowid DESC LIMIT 1",
                params![
                    source_id,
                    predicate,
                    target_id,
                    source_kind,
                    owner_id,
                    owner_id
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let mut merged_evidence: Vec<String> = evidence_ids
            .iter()
            .filter(|item| !item.is_empty())
            .cloned()
            .collect();
        let mut merged_confidence = confidence;
        if let Some((_, _, props_json, evidence_json, old_conf)) = &row {
            // 先移动 merged_props 所有权，再在本作用域重建。
            merged_props = merge_props(merged_props, props_json);
            // 轮2-R1-A⑤：存量 evidence 解析失败必须响亮报错——修前 unwrap_or_default()
            // 会把旧证据坍缩为空向量并由本函数 UPDATE 物理覆写，既往证据不可逆丢失：
            // 图的第一原则就是事件溯源不可伪造。
            let mut old_evidence: Vec<String> =
                serde_json::from_str(evidence_json).map_err(|err| {
                    StoreError::Repo(format!("存量边 evidence_json 解析失败（拒写防护）：{err}"))
                })?;
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
        // viewer_id 列口径（v5 json_extract 等价）：ai_semantic 严格等于
        // properties.viewer_id（缺省为空——v5 中 json_extract 返回 NULL，不参与
        // close_missing 匹配）；其他 source_kind 从 viewer: 前缀派生，供索引查询。
        let viewer_column = if source_kind == "ai_semantic" {
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
}
