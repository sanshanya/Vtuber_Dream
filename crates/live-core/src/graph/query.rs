//! 读路径（M3 工具面基元）：search_entities / references / episodes；
//! 面板聚合读：graph_stats / run_pair_delta（live-server 概览指标条消费）。
//!
//! 体积备书（轮3）：超 500 线 = 两族消费者同卷（LLM 工具读 + server 概览读），
//! 共 row_to_map/fetch_all 读原语。缝 = 概览统计收 `stats.rs`；真实需求到再动。
//!
//! 上限口径：全局 GRAPH_QUERY_LIMIT = 500（references 分块 / episodes 钳制），
//! search_entities 另封顶 100（Python parity，SEARCH_ENTITIES_LIMIT）。
//!
//! 轮2-R1-B2 头注纠偏：旧注「query()/project() 已删、待 M2/M3 复出」是 M1 时代的
//! 遗嘱——事实面早已复出：graph_stats/run_pair_delta 服役于 server 概览，
//! 全量投影像在 project.rs（独立模块，stats 段与本文件 graph_stats 共用
//! count_scalar，口径差异见各自注释：project 带 current_run_id 过滤，本文件为全存量）。

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

pub(crate) fn fetch_all(
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
            // graph 集成 M1：entities 桶必须与 nodes 镜像对齐——没有 Entity 节点的
            // 实体行（手迁/导入破坏镜像）不得过审（否则 accepted→落库 FK 崩分裂）。
            let sql = if table == "entities" {
                format!(
                    "SELECT e.entity_id FROM entities e \
                     JOIN nodes n ON n.node_id = e.entity_id AND n.node_type='Entity' \
                     WHERE e.entity_id IN ({placeholders})"
                )
            } else {
                format!("SELECT {column} FROM {table} WHERE {column} IN ({placeholders})")
            };
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
    // v7 起 episodes 带 lead_id 溯源列（G2）——显式列清单把它挡在读面之外
    //（本列尚无读面消费者；黄金对账与 tree 端点字段集保持原形状）。
    const EPISODE_COLUMNS: &str = "episode_id,viewer_id,source,event_type,observed_at,\
         published_at,title,url,bvid,fields_json,platform_facts_json,content_hash,\
         first_seen_at,last_seen_at";
    let (sql, args) = if viewer_id.is_empty() {
        (
            format!("SELECT {EPISODE_COLUMNS} FROM episodes ORDER BY observed_at DESC"),
            Vec::new(),
        )
    } else {
        (
            format!(
                "SELECT {EPISODE_COLUMNS} FROM episodes WHERE viewer_id=? ORDER BY observed_at DESC"
            ),
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
// mentions_of_viewer（M5 B2：/api/rooms/{uid}/viewers/{vid}/tree 的 mention 面）
// ---------------------------------------------------------------------------

/// 观众 mention 明细分组 + REFERS_TO 归属（左外联；排序与 project() 的 mention 面同齿）。
pub fn mentions_of_viewer(
    store: &Store,
    viewer_id: &str,
    limit: Option<i64>,
) -> Result<Vec<Value>> {
    let rows = fetch_all(
        store,
        "SELECT m.*,r.target_id AS entity_id, e.canonical_name,e.entity_type \
         FROM mentions m \
         LEFT JOIN edges r ON r.source_id=m.mention_id \
           AND r.predicate='REFERS_TO' AND r.source_kind='grounded_ai' AND r.valid_to IS NULL \
         LEFT JOIN entities e ON e.entity_id=r.target_id \
         WHERE m.viewer_id=? ORDER BY m.created_at DESC",
        vec![viewer_id.to_string().into()],
        limit.map(|v| v.clamp(1, GRAPH_QUERY_LIMIT)),
    )?;
    Ok(rows.into_iter().map(Value::Object).collect())
}

// M3-B query_graph 工具出生，聚合视图随之复出；逐行对齐 Python graph.py:689）
// ---------------------------------------------------------------------------

fn rename_key(map: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = map.remove(from) {
        map.insert(to.to_string(), value);
    }
}

/// 聚合检索：nodes + 关联活跃 edges + 边证据 mentions。
/// situation_run_id 存在时：Situation/Action 节点仅当它们在该运行中有活跃出边才可见。
pub fn query(
    store: &Store,
    needle: &str,
    node_types: &[String],
    predicates: &[String],
    limit: i64,
    situation_run_id: Option<&str>,
) -> Result<Value> {
    let bounded = limit.clamp(1, GRAPH_QUERY_LIMIT);
    let needle = needle.trim();

    // ---- nodes ----
    let mut node_where: Vec<String> = Vec::new();
    let mut node_params: Vec<Sql> = Vec::new();
    match situation_run_id {
        Some(run_id) => {
            node_where.push(
                "(n.node_type NOT IN ('Situation','Action') OR EXISTS (\
                    SELECT 1 FROM edges e \
                    WHERE e.source_id=n.node_id AND e.valid_to IS NULL AND e.run_id=?))"
                    .to_string(),
            );
            node_params.push(run_id.to_string().into());
        }
        None => node_where.push("n.node_type NOT IN ('Situation','Action')".to_string()),
    }
    let types: Vec<&str> = node_types
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if !types.is_empty() {
        node_where.push(format!(
            "n.node_type IN ({})",
            vec!["?"; types.len()].join(",")
        ));
        for t in &types {
            node_params.push(t.to_string().into());
        }
    }
    if !needle.is_empty() {
        node_where
            .push("(n.node_id LIKE ? OR n.name LIKE ? OR n.properties_json LIKE ?)".to_string());
        let like = format!("%{needle}%");
        node_params.extend([like.clone().into(), like.clone().into(), like.into()]);
    }
    let node_sql = format!(
        "SELECT * FROM nodes n WHERE {} ORDER BY n.node_type,n.name",
        node_where.join(" AND ")
    );
    let node_rows = fetch_all(store, &node_sql, node_params, Some(bounded))?;
    let mut nodes: Vec<Value> = Vec::new();
    let mut node_ids: Vec<String> = Vec::new();
    for mut row in node_rows {
        parse_json_field(&mut row, "properties_json");
        rename_key(&mut row, "node_id", "id");
        rename_key(&mut row, "node_type", "type");
        if let Some(Value::String(id)) = row.get("id") {
            node_ids.push(id.clone());
        }
        nodes.push(Value::Object(row));
    }

    // ---- edges ----
    let mut edge_where: Vec<String> = vec!["e.valid_to IS NULL".to_string()];
    let mut edge_params: Vec<Sql> = Vec::new();
    match situation_run_id {
        Some(run_id) => {
            edge_where.push(
                "NOT EXISTS (\
                    SELECT 1 FROM nodes n \
                    WHERE n.node_id IN (e.source_id,e.target_id) \
                      AND n.node_type IN ('Situation','Action') \
                      AND e.run_id<>?)"
                    .to_string(),
            );
            edge_params.push(run_id.to_string().into());
        }
        None => edge_where.push(
            "NOT EXISTS (\
                SELECT 1 FROM nodes n \
                WHERE n.node_id IN (e.source_id,e.target_id) \
                  AND n.node_type IN ('Situation','Action'))"
                .to_string(),
        ),
    }
    let predicate_values: Vec<&str> = predicates
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if !predicate_values.is_empty() {
        edge_where.push(format!(
            "e.predicate IN ({})",
            vec!["?"; predicate_values.len()].join(",")
        ));
        for p in &predicate_values {
            edge_params.push(p.to_string().into());
        }
    }
    let mut matches: Vec<String> = Vec::new();
    if !needle.is_empty() {
        matches.push("(e.predicate LIKE ? OR e.properties_json LIKE ?)".to_string());
        let like = format!("%{needle}%");
        edge_params.extend([like.clone().into(), like.into()]);
    }
    if !node_ids.is_empty() {
        let placeholders = vec!["?"; node_ids.len()].join(",");
        matches.push(format!(
            "(e.source_id IN ({placeholders}) OR e.target_id IN ({placeholders}))"
        ));
        for id in &node_ids {
            edge_params.push(id.clone().into());
        }
        for id in &node_ids {
            edge_params.push(id.clone().into());
        }
    }
    if !matches.is_empty() {
        edge_where.push(format!("({})", matches.join(" OR ")));
    }
    let edge_sql = format!(
        "SELECT * FROM edges e WHERE {} ORDER BY e.predicate,e.source_id,e.target_id",
        edge_where.join(" AND ")
    );
    let edge_rows = fetch_all(store, &edge_sql, edge_params, Some(bounded))?;
    let mut edges: Vec<Value> = Vec::new();
    let mut evidence_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for mut row in edge_rows {
        parse_json_field(&mut row, "properties_json");
        parse_json_field(&mut row, "evidence_json");
        rename_key(&mut row, "edge_id", "id");
        rename_key(&mut row, "source_id", "source");
        rename_key(&mut row, "target_id", "target");
        rename_key(&mut row, "evidence", "evidence_ids");
        if let Some(Value::Array(ids)) = row.get("evidence_ids") {
            for id in ids.iter().filter_map(Value::as_str) {
                if !id.is_empty() {
                    evidence_ids.insert(id.to_string());
                }
            }
        }
        edges.push(Value::Object(row));
    }

    // ---- mentions（边证据回填，排序去重后截断到 bounded；Python sorted(set)[:limit]）----
    let evidence_ids: Vec<String> = evidence_ids.into_iter().take(bounded as usize).collect();
    let mut mentions: Vec<Value> = Vec::new();
    if !evidence_ids.is_empty() {
        let placeholders = vec!["?"; evidence_ids.len()].join(",");
        let sql = format!("SELECT * FROM mentions WHERE mention_id IN ({placeholders})");
        let rows = fetch_all(
            store,
            &sql,
            evidence_ids.iter().cloned().map(Into::into).collect(),
            Some(bounded),
        )?;
        for row in rows {
            mentions.push(Value::Object(row));
        }
    }

    Ok(serde_json::json!({
        "nodes": nodes,
        "edges": edges,
        "mentions": mentions,
    }))
}

// ---------------------------------------------------------------------------
// run_pair_delta（M5 G4：/api/rooms/{uid}/overview「vs 上轮」delta 区块的取数源）
// ---------------------------------------------------------------------------

use serde_json::json;

/// 相邻两次 complete 运行的对照窗口取数（kickoff D3/D4）：
/// - 双运行定位 = graph_runs 中 completed_at 最近的两行；不足两行 → 基线态。
/// - interest 口径 = ai_state INTERESTED_IN 边在两端点时刻的 as-of 集合差分
///   （valid_from <= t AND (valid_to IS NULL OR valid_to > t)）；
/// - guards 口径 = GUARD_OF 边在 (from.completed_at, to.completed_at] 窗口内的开/闭。
///
/// 轮2-R1-B2 字序承重注：所有「时间比较」都是 TEXT 列上的字符串序——成立前提是
/// 全库时间戳统一 now_iso 形态（`%Y-%m-%dT%H:%M:%S.ffffff+00:00`，定长、UTC、零填充，
/// 字序≡时序）。任何写入面改用别的格式（如本地时区尾/无微秒段）都会静默撕裂
/// 这里的 as-of 语义——写时间戳只许走 now_iso/unix_secs_to_iso。
///
/// 首页指标条数据面（Z3：旧版 report 顶部数字条的透传口径——与 project.rs
/// 的 stats 段同款 COUNT SQL，「全存量」口径无 run_id 过滤）。`relations` = 全部
/// 当前有效边（valid_to IS NULL）；`interest_states` 为其中 AI 状态子集。
pub fn graph_stats(store: &Store) -> Result<Value> {
    let count = |sql: &str| store.count_scalar(sql, &[]);
    Ok(json!({
        "episodes": count("SELECT COUNT(*) FROM episodes")?,
        "mentions": count("SELECT COUNT(*) FROM mentions")?,
        "entities": count("SELECT COUNT(*) FROM nodes WHERE node_type='Entity'")?,
        "relations": count("SELECT COUNT(*) FROM edges WHERE valid_to IS NULL")?,
        "interest_states": count(
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' \
             AND source_kind='ai_state' AND valid_to IS NULL")?,
    }))
}

/// 键形（冻结给面板 DTO）——见模块测试：
/// `{ baseline_only, from_run_id, to_run_id,
///    interest: { opened, closed, changed }, guards: { added, removed } }`。
pub fn run_pair_delta(store: &Store) -> Result<Value> {
    // P0-4（复盘解耦）：recap-refresh 类 run（collect 尾的四个数刷新）不进对照窗——
    // 它只重放语料、零 AI 边；若放行，每日 collect 的 refresh 会把「vs 上轮感知」
    // 稀释成「无变化」。maintenance 类照旧参窗（它真实地动过边，排除即是谎报）。
    let sql = format!(
        "SELECT run_id, completed_at FROM graph_runs WHERE completed_at IS NOT NULL \
         AND kind != '{}' ORDER BY completed_at DESC, run_id DESC LIMIT 2",
        Store::RUN_KIND_RECAP_REFRESH
    );
    let runs = select_all(store, &sql, Vec::new())?;
    if runs.len() < 2 {
        return Ok(json!({
            "baseline_only": true,  // 面板显示「基线已建」
            "from_run_id": Value::Null,
            "to_run_id": Value::Null,
            "interest": {"opened": [], "closed": [], "changed": []},
            "guards": {"added": [], "removed": []},
        }));
    }
    let text = |row: &Map<String, Value>, key: &str| -> String {
        row.get(key)
            .and_then(Value::as_str)
            .expect("graph_runs 行完整")
            .to_string()
    };
    let (to_run_id, to_at) = (text(&runs[0], "run_id"), text(&runs[0], "completed_at"));
    let (from_run_id, from_at) = (text(&runs[1], "run_id"), text(&runs[1], "completed_at"));

    let interest_at = |at: &str| -> Result<Vec<Map<String, Value>>> {
        select_all(
            store,
            "SELECT e.source_id AS viewer_node, e.target_id, e.properties_json, \
               COALESCE(t.canonical_name, e.target_id) AS canonical_name \
             FROM edges e \
             LEFT JOIN entities t ON t.entity_id = e.target_id \
             WHERE e.predicate='INTERESTED_IN' AND e.source_kind='ai_state' \
               AND e.valid_from <= ? AND (e.valid_to IS NULL OR e.valid_to > ?) \
             ORDER BY e.source_id, e.target_id",
            vec![at.to_string().into(), at.to_string().into()],
        )
    };
    let from_rows = interest_at(&from_at)?;
    let to_rows = interest_at(&to_at)?;

    let props_value = |row: &Map<String, Value>| -> Value {
        row.get("properties_json")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or(Value::Null)
    };
    let row_item = |row: &Map<String, Value>| -> Value {
        let props = props_value(row);
        json!({
            "viewer_id": strip_viewer(row.get("viewer_node").and_then(Value::as_str).unwrap_or("")),
            "entity_id": row.get("target_id").cloned().unwrap_or(Value::Null),
            "canonical_name": row.get("canonical_name").cloned().unwrap_or(Value::Null),
            "status": props.get("status").cloned().unwrap_or(Value::Null),
            "preference": props.get("preference").cloned().unwrap_or(Value::Null),
        })
    };
    let mut from_map: std::collections::BTreeMap<String, &Map<String, Value>> =
        std::collections::BTreeMap::new();
    for row in &from_rows {
        from_map.insert(pair_key(row), row);
    }
    let mut to_map: std::collections::BTreeMap<String, &Map<String, Value>> =
        std::collections::BTreeMap::new();
    for row in &to_rows {
        to_map.insert(pair_key(row), row);
    }
    let mut opened = Vec::new();
    let mut closed = Vec::new();
    let mut changed = Vec::new();
    for (pair, row) in &to_map {
        match from_map.get(pair) {
            None => opened.push(row_item(row)),
            Some(old) if props_value(old) != props_value(row) => changed.push(json!({
                "viewer_id": strip_viewer(row.get("viewer_node").and_then(Value::as_str).unwrap_or("")),
                "entity_id": row.get("target_id").cloned().unwrap_or(Value::Null),
                "canonical_name": row.get("canonical_name").cloned().unwrap_or(Value::Null),
                "from": {
                    "status": props_value(old).get("status").cloned().unwrap_or(Value::Null),
                    "preference": props_value(old).get("preference").cloned().unwrap_or(Value::Null),
                },
                "to": {
                    "status": props_value(row).get("status").cloned().unwrap_or(Value::Null),
                    "preference": props_value(row).get("preference").cloned().unwrap_or(Value::Null),
                },
            })),
            Some(_) => {}
        }
    }
    for (pair, row) in &from_map {
        if !to_map.contains_key(pair) {
            closed.push(row_item(row));
        }
    }

    let guards = |window: &str, from: &str, to: &str| -> Result<Vec<Value>> {
        let rows = select_all(
            store,
            window,
            vec![from.to_string().into(), to.to_string().into()],
        )?;
        Ok(rows
            .iter()
            .map(|row| {
                Value::String(
                    strip_viewer(row.get("source_id").and_then(Value::as_str).unwrap_or(""))
                        .to_string(),
                )
            })
            .collect())
    };
    let added = guards(
        "SELECT source_id FROM edges \
         WHERE predicate='GUARD_OF' AND valid_to IS NULL \
           AND valid_from > ? AND valid_from <= ? ORDER BY source_id",
        &from_at,
        &to_at,
    )?;
    let removed = guards(
        "SELECT source_id FROM edges \
         WHERE predicate='GUARD_OF' AND valid_to IS NOT NULL \
           AND valid_to > ? AND valid_to <= ? ORDER BY source_id",
        &from_at,
        &to_at,
    )?;

    Ok(json!({
        "baseline_only": false,
        "from_run_id": from_run_id,
        "to_run_id": to_run_id,
        "interest": {
            "opened": opened,
            "closed": closed,
            "changed": changed,
        },
        "guards": {
            "added": added,
            "removed": removed,
        },
    }))
}

fn strip_viewer(node: &str) -> &str {
    node.strip_prefix("viewer:").unwrap_or(node)
}

fn pair_key(row: &Map<String, Value>) -> String {
    format!(
        "{}|{}",
        row.get("viewer_node").and_then(Value::as_str).unwrap_or(""),
        row.get("target_id").and_then(Value::as_str).unwrap_or("")
    )
}
