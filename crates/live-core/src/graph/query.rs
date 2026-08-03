//! 读路径（M3 工具面基元）：search_entities / references / episodes。
//!
//! 上限口径：全局 GRAPH_QUERY_LIMIT = 500（references 分块 / episodes 钳制），
//! search_entities 另封顶 100（Python parity，SEARCH_ENTITIES_LIMIT）。
//!
//! query()/project() 聚合视图在 M1 瘦身时删除（零消费者，YAGNI）；
//! M2 live-server / M3 报告层需要 evidence 类型化回填（§8.1）与 as_of 投影（§8.3）时，
//! 再按届时真实调用形态复出。

use serde_json::{Map, Value};

use crate::episodes::norm;
use crate::graph::store::{GRAPH_QUERY_LIMIT, Result, Store};

type Sql = rusqlite::types::Value;

fn row_to_map(row: &rusqlite::Row, columns: &[String]) -> rusqlite::Result<Map<String, Value>> {
    let mut map = Map::new();
    for (index, column) in columns.iter().enumerate() {
        let value: Sql = row.get(index)?;
        map.insert(
            column.clone(),
            match value {
                Sql::Null => Value::Null,
                Sql::Integer(n) => Value::from(n),
                Sql::Real(f) => serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                Sql::Text(s) => Value::String(s),
                Sql::Blob(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
            },
        );
    }
    Ok(map)
}

fn select_all(store: &Store, sql: &str, args: Vec<Sql>) -> Result<Vec<Map<String, Value>>> {
    let mut stmt = store.conn.prepare(sql)?;
    let columns: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), |row| {
            row_to_map(row, &columns)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn fetch_all(
    store: &Store,
    sql: &str,
    mut params: Vec<Sql>,
    limit: Option<i64>,
) -> Result<Vec<Map<String, Value>>> {
    let sql = match limit {
        Some(bound) => {
            params.push(bound.max(1).into());
            format!("{sql} LIMIT ?")
        }
        None => sql.to_string(),
    };
    select_all(store, &sql, params)
}

/// 把 `*_json` 文本列解析成 JSON 值并改名为去后缀键（Python dict(row) + pop 语义）。
fn parse_json_field(map: &mut Map<String, Value>, column: &str) {
    if let Some(Value::String(text)) = map.get(column).cloned() {
        map.insert(
            column.replace("_json", ""),
            serde_json::from_str(&text).unwrap_or(Value::Null),
        );
        map.remove(column);
    }
}

// ---------------------------------------------------------------------------
// search_entities（移植 repo.search_entities）
// ---------------------------------------------------------------------------

/// Python parity：search_entities 结果上限（图谱查询全局上限 GRAPH_QUERY_LIMIT 的另一口径）。
const SEARCH_ENTITIES_LIMIT: i64 = 100;

pub fn search_entities(
    store: &Store,
    query: &str,
    entity_type: &str,
    limit: i64,
) -> Result<Vec<Value>> {
    let needle = norm(query);
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let bounded = limit.clamp(1, SEARCH_ENTITIES_LIMIT);
    let like = format!("%{needle}%");
    let rows = select_all(
        store,
        "SELECT e.entity_id,e.canonical_name,e.entity_type,e.description,e.properties_json, \
                GROUP_CONCAT(a.alias, ' | ') AS aliases \
         FROM entities e \
         LEFT JOIN entity_aliases a ON a.entity_id=e.entity_id \
         WHERE (e.normalized_name LIKE ? OR a.alias_key LIKE ?) AND (?='' OR e.entity_type=?) \
         GROUP BY e.entity_id \
         ORDER BY CASE WHEN e.normalized_name=? THEN 0 ELSE 1 END, e.last_seen_at DESC \
         LIMIT ?",
        vec![
            like.clone().into(),
            like.into(),
            entity_type.to_string().into(),
            entity_type.to_string().into(),
            needle.into(),
            bounded.into(),
        ],
    )?;
    let mut result = Vec::new();
    for row in rows {
        let aliases: Vec<Value> = row
            .get("aliases")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split('|')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| Value::String(item.to_string()))
            .collect();
        let properties = row
            .get("properties_json")
            .and_then(Value::as_str)
            .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
            .unwrap_or(Value::Null);
        result.push(serde_json::json!({
            "entity_id": row.get("entity_id").cloned().unwrap_or(Value::Null),
            "canonical_name": row.get("canonical_name").cloned().unwrap_or(Value::Null),
            "entity_type": row.get("entity_type").cloned().unwrap_or(Value::Null),
            "description": row.get("description").cloned().unwrap_or(Value::String(String::new())),
            "aliases": aliases,
            "properties": properties,
        }));
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// references / episodes
// ---------------------------------------------------------------------------

pub fn references(
    store: &Store,
    entity_ids: &[String],
    episode_ids: &[String],
    mention_ids: &[String],
) -> Result<std::collections::HashMap<String, std::collections::BTreeSet<String>>> {
    let mut resolved = std::collections::HashMap::new();
    for (key, table, column, values) in [
        ("entities", "entities", "entity_id", entity_ids),
        ("episodes", "episodes", "episode_id", episode_ids),
        ("mentions", "mentions", "mention_id", mention_ids),
    ] {
        let ordered: Vec<String> = values
            .iter()
            .filter(|value| !value.is_empty())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut found = std::collections::BTreeSet::new();
        for chunk in ordered.chunks(GRAPH_QUERY_LIMIT as usize) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!("SELECT {column} FROM {table} WHERE {column} IN ({placeholders})");
            let rows = select_all(store, &sql, chunk.iter().cloned().map(Into::into).collect())?;
            for row in rows {
                if let Some(Value::String(id)) = row.get(column) {
                    found.insert(id.clone());
                }
            }
        }
        resolved.insert(key.to_string(), found);
    }
    Ok(resolved)
}

pub fn episodes(store: &Store, viewer_id: &str, limit: Option<i64>) -> Result<Vec<Value>> {
    let (sql, args) = if viewer_id.is_empty() {
        (
            "SELECT * FROM episodes ORDER BY observed_at DESC".to_string(),
            Vec::new(),
        )
    } else {
        (
            "SELECT * FROM episodes WHERE viewer_id=? ORDER BY observed_at DESC".to_string(),
            vec![viewer_id.to_string().into()],
        )
    };
    let rows = fetch_all(
        store,
        &sql,
        args,
        limit.map(|v| v.clamp(1, GRAPH_QUERY_LIMIT)),
    )?;
    let mut result = Vec::new();
    for mut row in rows {
        parse_json_field(&mut row, "fields_json");
        parse_json_field(&mut row, "platform_facts_json");
        result.push(Value::Object(row));
    }
    Ok(result)
}
