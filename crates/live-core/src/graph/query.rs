//! 读路径：search_entities / references / episodes / query / project。
//!
//! 差异说明：
//! - evidence 类型判别（v6 §8.1）：query() 按 id 前缀把 evidence 分派为
//!   mention / episode / search_result / other，mention 回 mentions 表、
//!   episode 回 episodes 表——修复 Python 现状"episode 证据在查询响应里静默丢失"。
//! - project() 增加 as_of（§8.3）：给出时点时边条件为
//!   `valid_from <= as_of AND (valid_to IS NULL OR valid_to > as_of)`。
//! - detect_communities（greedy modularity）未移植：M5 报告里程碑再对齐
//!   networkx 语义；当前 project 返回 communities: [] 且 stats.communities=0。

use rusqlite::params;
use serde_json::{Map, Value};

use crate::episodes::norm;
use crate::graph::store::{GRAPH_QUERY_LIMIT, Result, Store};

type Sql = rusqlite::types::Value;

fn opt_sql(value: &Option<String>) -> Sql {
    match value {
        Some(text) => text.clone().into(),
        None => Sql::Null,
    }
}

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
    let bounded = limit.clamp(1, GRAPH_QUERY_LIMIT);
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

// ---------------------------------------------------------------------------
// query()：节点搜索 + 关联边 + evidence 类型化回填（v6 修复）
// ---------------------------------------------------------------------------

/// evidence id 的类型判别（§8.1）。
pub fn evidence_kind(id: &str) -> &'static str {
    if id.starts_with("mention:") {
        "mention"
    } else if id.starts_with("episode:") {
        "episode"
    } else if id.starts_with("search:") || id.starts_with("result:") {
        "search_result"
    } else {
        "other"
    }
}

#[derive(Debug, Default)]
pub struct QueryOptions {
    pub node_types: Vec<String>,
    pub predicates: Vec<String>,
    pub limit: Option<i64>,
    pub situation_run_id: Option<String>,
}

pub struct QueryOutput {
    pub nodes: Vec<Value>,
    pub edges: Vec<Value>,
    pub mentions: Vec<Value>,
    pub episodes: Vec<Value>,
}

pub fn query(store: &Store, needle: &str, options: &QueryOptions) -> Result<QueryOutput> {
    let bounded = options
        .limit
        .unwrap_or(GRAPH_QUERY_LIMIT)
        .clamp(1, GRAPH_QUERY_LIMIT);
    let mut node_where: Vec<String> = Vec::new();
    let mut node_params: Vec<Sql> = Vec::new();
    if let Some(situation_run_id) = &options.situation_run_id {
        node_where.push(
            "(n.node_type NOT IN ('Situation','Action') OR EXISTS ( \
               SELECT 1 FROM edges e \
               WHERE e.source_id=n.node_id AND e.valid_to IS NULL AND e.run_id=?))"
                .to_string(),
        );
        node_params.push(situation_run_id.clone().into());
    } else {
        node_where.push("n.node_type NOT IN ('Situation','Action')".to_string());
    }
    let types: Vec<String> = options
        .node_types
        .iter()
        .filter(|item| !item.trim().is_empty())
        .cloned()
        .collect();
    if !types.is_empty() {
        node_where.push(format!(
            "n.node_type IN ({})",
            vec!["?"; types.len()].join(",")
        ));
        node_params.extend(types.into_iter().map(Into::into));
    }
    let trimmed = needle.trim();
    if !trimmed.is_empty() {
        node_where
            .push("(n.node_id LIKE ? OR n.name LIKE ? OR n.properties_json LIKE ?)".to_string());
        let like = format!("%{trimmed}%");
        node_params.extend([like.clone().into(), like.clone().into(), like.into()]);
    }
    let node_sql = format!(
        "SELECT * FROM nodes n WHERE {} ORDER BY n.node_type,n.name LIMIT ?",
        node_where.join(" AND ")
    );
    node_params.push(bounded.into());
    let node_rows = select_all(store, &node_sql, node_params)?;
    let mut nodes = Vec::new();
    for mut row in node_rows {
        parse_json_field(&mut row, "properties_json");
        if let Some(id) = row.remove("node_id") {
            row.insert("id".to_string(), id);
        }
        if let Some(kind) = row.remove("node_type") {
            row.insert("type".to_string(), kind);
        }
        nodes.push(Value::Object(row));
    }

    let mut edge_where = vec!["e.valid_to IS NULL".to_string()];
    let mut edge_params: Vec<Sql> = Vec::new();
    if let Some(situation_run_id) = &options.situation_run_id {
        edge_where.push(
            "NOT EXISTS (SELECT 1 FROM nodes n \
               WHERE n.node_id IN (e.source_id,e.target_id) \
                 AND n.node_type IN ('Situation','Action') AND e.run_id<>?)"
                .to_string(),
        );
        edge_params.push(situation_run_id.clone().into());
    } else {
        edge_where.push(
            "NOT EXISTS (SELECT 1 FROM nodes n \
               WHERE n.node_id IN (e.source_id,e.target_id) \
                 AND n.node_type IN ('Situation','Action'))"
                .to_string(),
        );
    }
    let predicates: Vec<String> = options
        .predicates
        .iter()
        .filter(|item| !item.trim().is_empty())
        .cloned()
        .collect();
    if !predicates.is_empty() {
        edge_where.push(format!(
            "e.predicate IN ({})",
            vec!["?"; predicates.len()].join(",")
        ));
        edge_params.extend(predicates.into_iter().map(Into::into));
    }
    let node_ids: Vec<String> = nodes
        .iter()
        .filter_map(|node| node.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut matches: Vec<String> = Vec::new();
    if !trimmed.is_empty() {
        matches.push("(e.predicate LIKE ? OR e.properties_json LIKE ?)".to_string());
        let like = format!("%{trimmed}%");
        edge_params.extend([like.clone().into(), like.into()]);
    }
    if !node_ids.is_empty() {
        let placeholders = vec!["?"; node_ids.len()].join(",");
        matches.push(format!(
            "(e.source_id IN ({placeholders}) OR e.target_id IN ({placeholders}))"
        ));
        edge_params.extend(node_ids.iter().cloned().map(Into::into));
        edge_params.extend(node_ids.iter().cloned().map(Into::into));
    }
    if !matches.is_empty() {
        edge_where.push(format!("({})", matches.join(" OR ")));
    }
    let edge_sql = format!(
        "SELECT * FROM edges e WHERE {} ORDER BY e.predicate,e.source_id,e.target_id LIMIT ?",
        edge_where.join(" AND ")
    );
    edge_params.push(bounded.into());
    let edge_rows = select_all(store, &edge_sql, edge_params)?;
    let mut edges = Vec::new();
    let mut evidence_ids = std::collections::BTreeSet::new();
    for mut row in edge_rows {
        if let Some(id) = row.remove("edge_id") {
            row.insert("id".to_string(), id);
        }
        if let Some(source) = row.remove("source_id") {
            row.insert("source".to_string(), source);
        }
        if let Some(target) = row.remove("target_id") {
            row.insert("target".to_string(), target);
        }
        parse_json_field(&mut row, "properties_json");
        if let Some(Value::String(text)) = row.remove("evidence_json") {
            let ids: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
            for id in &ids {
                if !id.is_empty() {
                    evidence_ids.insert(id.clone());
                }
            }
            row.insert(
                "evidence_ids".to_string(),
                Value::Array(ids.into_iter().map(Value::String).collect()),
            );
        }
        edges.push(Value::Object(row));
    }
    let ordered: Vec<String> = evidence_ids.into_iter().take(bounded as usize).collect();

    // v6：按类型分派回填 mention / episode 表（episode 证据不再丢失）。
    let mention_ids: Vec<String> = ordered
        .iter()
        .filter(|id| evidence_kind(id) == "mention")
        .cloned()
        .collect();
    let episode_ids: Vec<String> = ordered
        .iter()
        .filter(|id| evidence_kind(id) == "episode")
        .cloned()
        .collect();
    let mut mentions = Vec::new();
    if !mention_ids.is_empty() {
        let placeholders = vec!["?"; mention_ids.len()].join(",");
        let rows = select_all(
            store,
            &format!("SELECT * FROM mentions WHERE mention_id IN ({placeholders}) LIMIT {bounded}"),
            mention_ids.into_iter().map(Into::into).collect(),
        )?;
        mentions = rows.into_iter().map(Value::Object).collect();
    }
    let mut episode_rows = Vec::new();
    if !episode_ids.is_empty() {
        let placeholders = vec!["?"; episode_ids.len()].join(",");
        let rows = select_all(
            store,
            &format!("SELECT * FROM episodes WHERE episode_id IN ({placeholders}) LIMIT {bounded}"),
            episode_ids.into_iter().map(Into::into).collect(),
        )?;
        for mut row in rows {
            parse_json_field(&mut row, "fields_json");
            parse_json_field(&mut row, "platform_facts_json");
            episode_rows.push(Value::Object(row));
        }
    }
    Ok(QueryOutput {
        nodes,
        edges,
        mentions,
        episodes: episode_rows,
    })
}

// ---------------------------------------------------------------------------
// project()：整图投影 DTO（含 as_of；communities 待 M5）
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct ProjectOptions {
    pub include_episodes: bool,
    pub include_interest_states: bool,
    pub limit: Option<i64>,
    pub include_situation_actions: bool,
    pub situation_run_id: Option<String>,
    pub current_run_id: Option<String>,
    pub as_of: Option<String>,
}

impl ProjectOptions {
    pub fn full() -> Self {
        Self {
            include_episodes: true,
            include_interest_states: true,
            include_situation_actions: true,
            ..Self::default()
        }
    }
}

pub fn project(store: &Store, options: &ProjectOptions) -> Result<Value> {
    let limit = options.limit;
    let current = &options.current_run_id;

    // ---- nodes ----
    let (node_sql, node_params): (String, Vec<Sql>) = if !options.include_situation_actions {
        (
            "SELECT * FROM nodes WHERE node_type NOT IN ('Situation','Action') \
             ORDER BY node_type,name"
                .to_string(),
            Vec::new(),
        )
    } else if let Some(situation_run_id) = &options.situation_run_id {
        (
            "SELECT * FROM nodes n WHERE n.node_type NOT IN ('Situation','Action') OR EXISTS ( \
               SELECT 1 FROM edges e \
               WHERE e.source_id=n.node_id AND e.valid_to IS NULL AND e.run_id=?) \
             ORDER BY node_type,name"
                .to_string(),
            vec![situation_run_id.clone().into()],
        )
    } else {
        (
            "SELECT * FROM nodes ORDER BY node_type,name".to_string(),
            Vec::new(),
        )
    };

    // ---- edges ----
    let mut edge_where: Vec<String> = Vec::new();
    let mut edge_params: Vec<Sql> = Vec::new();
    match &options.as_of {
        Some(as_of) => {
            edge_where.push(format!(
                "({AS}.valid_from <= ? AND ({AS}.valid_to IS NULL OR {AS}.valid_to > ?))",
                AS = "e"
            ));
            edge_params.extend([as_of.clone().into(), as_of.clone().into()]);
        }
        None => edge_where.push("e.valid_to IS NULL".to_string()),
    }
    edge_where.push(
        "(? IS NULL OR e.source_kind NOT IN ('ai_state','ai_semantic') OR e.run_id=?)".to_string(),
    );
    edge_params.push(opt_sql(current));
    edge_params.push(opt_sql(current));
    if !options.include_situation_actions {
        edge_where.push(
            "NOT EXISTS (SELECT 1 FROM nodes n \
               WHERE n.node_id IN (e.source_id,e.target_id) \
                 AND n.node_type IN ('Situation','Action'))"
                .to_string(),
        );
    } else if let Some(situation_run_id) = &options.situation_run_id {
        edge_where.push(
            "NOT EXISTS (SELECT 1 FROM nodes n \
               WHERE n.node_id IN (e.source_id,e.target_id) \
                 AND n.node_type IN ('Situation','Action') AND e.run_id<>?)"
                .to_string(),
        );
        edge_params.push(situation_run_id.clone().into());
    }
    let edge_sql = format!(
        "SELECT * FROM edges e WHERE {} ORDER BY predicate,source_id,target_id",
        edge_where.join(" AND ")
    );

    let mut nodes = Vec::new();
    for mut row in fetch_all(store, &node_sql, node_params, limit)? {
        parse_json_field(&mut row, "properties_json");
        if let Some(id) = row.remove("node_id") {
            row.insert("id".to_string(), id);
        }
        if let Some(kind) = row.remove("node_type") {
            row.insert("type".to_string(), kind);
        }
        nodes.push(Value::Object(row));
    }
    let mut edges = Vec::new();
    for mut row in fetch_all(store, &edge_sql, edge_params, limit)? {
        if let Some(id) = row.remove("edge_id") {
            row.insert("id".to_string(), id);
        }
        if let Some(source) = row.remove("source_id") {
            row.insert("source".to_string(), source);
        }
        if let Some(target) = row.remove("target_id") {
            row.insert("target".to_string(), target);
        }
        parse_json_field(&mut row, "properties_json");
        if let Some(Value::String(text)) = row.remove("evidence_json") {
            let ids: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
            row.insert(
                "evidence_ids".to_string(),
                Value::Array(ids.into_iter().map(Value::String).collect()),
            );
        }
        edges.push(Value::Object(row));
    }

    let mentions_sql = "SELECT m.*,r.target_id AS entity_id, \
             json_extract(r.properties_json, '$.decision') AS decision, \
             e.canonical_name,e.entity_type \
         FROM mentions m \
         LEFT JOIN edges r ON r.source_id=m.mention_id \
           AND r.predicate='REFERS_TO' AND r.source_kind='grounded_ai' AND r.valid_to IS NULL \
         LEFT JOIN entities e ON e.entity_id=r.target_id \
         ORDER BY m.created_at DESC";
    let mentions: Vec<Value> = fetch_all(store, mentions_sql, Vec::new(), limit)?
        .into_iter()
        .map(Value::Object)
        .collect();

    let count = |sql: &str| -> Result<i64> {
        store
            .conn
            .query_row(sql, [], |row| row.get(0))
            .map_err(Into::into)
    };
    let interest_count: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' \
         AND source_kind='ai_state' AND valid_to IS NULL \
         AND (? IS NULL OR run_id=?)",
        params![opt_sql(current), opt_sql(current)],
        |row| row.get(0),
    )?;
    // TODO(M5): detect_communities 对齐 networkx greedy_modularity 语义后补上；
    // 当前返回空列表（设计文档 M1 验收不含社区，M5 报告里程碑再补齐）。
    let communities: Vec<Value> = Vec::new();
    let mut graph = Map::new();
    graph.insert("schema_version".to_string(), Value::from(6));
    graph.insert("generated_at".to_string(), Value::String(store.now()));
    graph.insert(
        "stats".to_string(),
        serde_json::json!({
            "nodes": nodes.len() as i64,
            "edges": edges.len() as i64,
            "episodes": count("SELECT COUNT(*) FROM episodes")?,
            "mentions": count("SELECT COUNT(*) FROM mentions")?,
            "entities": count("SELECT COUNT(*) FROM nodes WHERE node_type='Entity'")?,
            "interest_states": interest_count,
            "communities": communities.len() as i64,
        }),
    );
    graph.insert("nodes".to_string(), Value::Array(nodes));
    graph.insert("edges".to_string(), Value::Array(edges));
    graph.insert("mentions".to_string(), Value::Array(mentions));
    graph.insert("communities".to_string(), Value::Array(communities));
    if options.include_episodes {
        graph.insert(
            "episodes".to_string(),
            Value::Array(episodes(store, "", limit)?),
        );
    }
    if options.include_interest_states {
        let states_sql = "SELECT s.*,e.canonical_name,e.entity_type \
             FROM edges s JOIN entities e ON e.entity_id=s.target_id \
             WHERE s.predicate='INTERESTED_IN' AND s.source_kind='ai_state' \
               AND s.valid_to IS NULL AND (? IS NULL OR s.run_id=?) \
             ORDER BY s.source_id,s.confidence DESC";
        let mut states = Vec::new();
        for mut row in fetch_all(
            store,
            states_sql,
            vec![opt_sql(current), opt_sql(current)],
            limit,
        )? {
            if let Some(Value::String(text)) = row.remove("properties_json")
                && let Value::Object(props) = serde_json::from_str(&text).unwrap_or(Value::Null)
            {
                for (key, value) in props {
                    row.insert(key, value);
                }
            }
            if let Some(Value::String(text)) = row.remove("evidence_json") {
                row.insert(
                    "evidence_mention_ids".to_string(),
                    serde_json::from_str(&text).unwrap_or(Value::Array(Vec::new())),
                );
            }
            if let Some(id) = row.remove("edge_id") {
                row.insert("state_id".to_string(), id);
            }
            if let Some(Value::String(source)) = row.remove("source_id") {
                row.insert(
                    "viewer_id".to_string(),
                    Value::String(
                        source
                            .strip_prefix("viewer:")
                            .unwrap_or(&source)
                            .to_string(),
                    ),
                );
            }
            if let Some(target) = row.remove("target_id") {
                row.insert("entity_id".to_string(), target);
            }
            states.push(Value::Object(row));
        }
        graph.insert("interest_states".to_string(), Value::Array(states));
    }
    Ok(Value::Object(graph))
}
