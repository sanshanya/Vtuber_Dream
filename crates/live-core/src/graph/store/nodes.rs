//! 节点 upsert：通用 nodes 表 + 平台实体（entities 表 + Entity 节点 + 平台别名）。

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};

use crate::episodes::{json_canon, norm};

use super::{Result, Store, merge_props};

impl Store {
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
            merged = merge_props(merged, &props_json);
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
        if let Some((_, props_json)) = row {
            merged = merge_props(merged, &props_json);
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
}
