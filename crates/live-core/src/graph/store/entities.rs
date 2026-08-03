//! 实体解析（SAME_AS / NEW_ENTITY / UNCERTAIN）与别名幂等写入。

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};

use crate::episodes::{hash_parts, json_canon, norm, py_repr_list, safe_type};
use crate::models::EntityProposal;

use super::{Result, Store, merge_props, repo_err};

impl Store {
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

    /// 别名幂等写入（ai 来源）：alias_key=norm(alias)，冲突时 confidence 取 max。
    fn upsert_alias(&self, entity_id: &str, alias: &str, confidence: f64, now: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO entity_aliases(alias_key,entity_id,alias,source_kind,confidence,created_at) \
             VALUES(?,?,?,?,?,?) \
             ON CONFLICT(alias_key,entity_id) DO UPDATE SET \
               alias=excluded.alias,confidence=MAX(entity_aliases.confidence,excluded.confidence)",
            params![norm(alias), entity_id, alias, "ai", confidence, now],
        )?;
        Ok(())
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
                self.upsert_alias(&existing, &alias, proposal.confidence, &now)?;
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
        if let Some((_, props_json)) = row {
            properties = merge_props(properties, &props_json);
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
            self.upsert_alias(&resolved, &alias, proposal.confidence, &now)?;
        }
        Ok((resolved, decision))
    }
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
