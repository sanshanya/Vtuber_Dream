//! project() 聚合导出 + detect_communities()（移植 graph.py:849-1038）。
//!
//! detect_communities 是 networkx `greedy_modularity_communities`（CNM）的 O(n²) 扫实现：
//! 合并序列由 (dq desc, (u,v) 码元序) 全序决定（MappedQueue (priority, element) 对比较语义），
//! 与 networkx 逐合并一致；社区输出序：提前停（dq<0）= 插入序；耗尽（连通分量全并）= 尺寸降序。
//! golden 对账：tests/graph_project.rs × tests-fixtures/m4b/。
//!
//! 复刻前提：输入边的 (source,target) 在「活跃 INTERESTED_IN」上唯一（应用层 upsert 幂等
//! 保证；networkx `add_edge` 覆盖语义）。层内不做去重——出现重复活跃边时本实现累加权重、
//! Python 取最后一条，属登记在案的病态分叉（parity_negative 有记录测试）。

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use super::query;
use super::store::{GRAPH_SCHEMA_VERSION, Result, Store};

#[derive(Debug, Clone)]
pub struct ProjectOptions {
    pub include_episodes: bool,
    pub include_interest_states: bool,
    pub include_situation_actions: bool,
    pub situation_run_id: Option<String>,
    pub current_run_id: Option<String>,
    pub limit: Option<i64>,
    pub minimum_community_size: i64,
}

impl Default for ProjectOptions {
    fn default() -> Self {
        Self {
            include_episodes: true,
            include_interest_states: true,
            include_situation_actions: true,
            situation_run_id: None,
            current_run_id: None,
            limit: None,
            minimum_community_size: 1,
        }
    }
}

fn sql_arg(value: Option<&str>) -> rusqlite::types::Value {
    match value {
        Some(text) => rusqlite::types::Value::Text(text.to_string()),
        None => rusqlite::types::Value::Null,
    }
}

// ---------------------------------------------------------------------------
// detect_communities（networkx CNM 移植）
// ---------------------------------------------------------------------------

/// Python `float(edge.get("confidence") or 0.5)`：`None/0/""` 等 falsy → 0.5。
fn edge_weight(confidence: Option<&Value>) -> f64 {
    match confidence {
        Some(Value::Number(n)) => {
            let w = n.as_f64().unwrap_or(0.0);
            if w == 0.0 { 0.5 } else { w }
        }
        Some(Value::String(s)) => {
            let w = s.parse::<f64>().unwrap_or(0.0);
            if w == 0.0 { 0.5 } else { w }
        }
        _ => 0.5,
    }
}

/// 合并候选：(dq，(u,v))；全局最小 = dq 最大、平局 (u,v) 码元序最小。
type MergeKey = (f64, String, String);

fn better(left: &MergeKey, right: &MergeKey) -> std::cmp::Ordering {
    right
        .0
        .total_cmp(&left.0)
        .then_with(|| left.1.cmp(&right.1))
        .then_with(|| left.2.cmp(&right.2))
}

/// Python `detect_communities`：只对 INTERESTED_IN 构图（viewer+Entity 混合点集）。
pub fn detect_communities(nodes: &[Value], edges: &[Value], minimum_size: i64) -> Vec<Value> {
    let mut name_lookup: HashMap<String, String> = HashMap::new();
    let mut type_lookup: HashMap<String, String> = HashMap::new();
    for node in nodes {
        let id = node
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        name_lookup.insert(
            id.clone(),
            match node.get("name").and_then(Value::as_str) {
                Some(name) if !name.is_empty() => name.to_string(),
                _ => id.clone(),
            },
        );
        type_lookup.insert(
            id.clone(),
            node.get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
    }
    // 节点插入序 = 边列表首现序（先 source 后 target；networkx add_edge 语义）。
    let mut node_order: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pairs: Vec<(String, String, f64)> = Vec::new();
    for edge in edges {
        if edge.get("predicate").and_then(Value::as_str) != Some("INTERESTED_IN") {
            continue;
        }
        let source = edge.get("source").and_then(Value::as_str).unwrap_or("");
        let target = edge.get("target").and_then(Value::as_str).unwrap_or("");
        let weight = edge_weight(edge.get("confidence"));
        for node in [source, target] {
            if seen.insert(node.to_string()) {
                node_order.push(node.to_string());
            }
        }
        pairs.push((source.to_string(), target.to_string(), weight));
    }
    if pairs.is_empty() {
        return Vec::new();
    }
    // 已知病态分叉，登记不修复：pairs 可含同 (u,v) 多条活跃边，本实现
    // m/a/weight_pairs 全部累加；networkx `G.add_edge` 覆盖权重。正常路径不可达
    // （应用层 upsert 幂等），仅手迁 SQL 可造——parity_negative 有记录测试。
    // 已知病态分叉：m=0（需负 confidence 列，应用层检查挡在 0.7+）
    // 时 Python `1/m` 抛 ZeroDivisionError，本实现 q0=inf 继续产出。
    let m: f64 = pairs.iter().map(|(_, _, w)| w).sum();
    let q0 = 1.0 / m;
    let mut a: HashMap<String, f64> = HashMap::new();
    for (u, v, w) in &pairs {
        *a.entry(u.clone()).or_insert(0.0) += w;
        *a.entry(v.clone()).or_insert(0.0) += w;
    }
    for value in a.values_mut() {
        *value *= q0 * 0.5;
    }
    let a_of = |a: &HashMap<String, f64>, node: &str| a.get(node).copied().unwrap_or(0.0);
    // dq_dict[u][v]（双向镜像；u==v 自环跳过）。
    let mut dq: HashMap<(String, String), f64> = HashMap::new();
    let mut weight_pairs: HashMap<(String, String), f64> = HashMap::new();
    for (u, v, w) in &pairs {
        if u == v {
            continue;
        }
        *weight_pairs.entry((u.clone(), v.clone())).or_insert(0.0) += w;
        *weight_pairs.entry((v.clone(), u.clone())).or_insert(0.0) += w;
    }
    for ((u, v), w) in weight_pairs {
        let value = q0 * w - (a_of(&a, &u) * a_of(&a, &v) + a_of(&a, &u) * a_of(&a, &v));
        dq.insert((u, v), value);
    }
    // communities：插入序 key 表 + 成员集（dict/del 语义 → Vec sweep 保持位序）。
    let mut key_order: Vec<String> = node_order.clone();
    let mut members: HashMap<String, BTreeSet<String>> = node_order
        .iter()
        .map(|n| {
            let mut set = BTreeSet::new();
            set.insert(n.clone());
            (n.clone(), set)
        })
        .collect();
    loop {
        let rows_with_candidates = key_order
            .iter()
            .filter(|u| dq.keys().any(|(r, _)| r == *u))
            .count();
        if rows_with_candidates <= 1 || key_order.len() <= 1 {
            // 耗尽路径（networkx StopIteration）。
            break;
        }
        // 全局最优合并对：min by (dq desc, u asc, v asc)。
        let mut best: Option<MergeKey> = None;
        for ((u, v), value) in &dq {
            let key = (*value, u.clone(), v.clone());
            if best
                .as_ref()
                .is_none_or(|b| better(&key, b) == std::cmp::Ordering::Less)
            {
                best = Some(key);
            }
        }
        let Some((dq_best, u, v)) = best else {
            break;
        };
        // wrapper：dq < 0 且已达 best_n(=N) → 提前停（保持插入序）。
        if dq_best < 0.0 {
            break;
        }
        // u 并入 v
        let u_members = members.remove(&u).expect("merge source exists");
        members
            .get_mut(&v)
            .expect("merge target exists")
            .extend(u_members);
        key_order.retain(|key| key != &u);
        let u_nbrs: BTreeSet<String> = dq
            .keys()
            .filter(|(r, _)| r == &u)
            .map(|(_, c)| c.clone())
            .collect();
        let v_nbrs: BTreeSet<String> = dq
            .keys()
            .filter(|(r, _)| r == &v)
            .map(|(_, c)| c.clone())
            .collect();
        let all_nbrs: Vec<String> = u_nbrs
            .union(&v_nbrs)
            .filter(|n| *n != &u && *n != &v)
            .cloned()
            .collect();
        for w in &all_nbrs {
            let in_u = u_nbrs.contains(w);
            let in_v = v_nbrs.contains(w);
            let value = if in_u && in_v {
                dq[&(v.clone(), w.clone())] + dq[&(u.clone(), w.clone())]
            } else if in_v {
                dq[&(v.clone(), w.clone())]
                    - (a_of(&a, &u) * a_of(&a, w) + a_of(&a, w) * a_of(&a, &u))
            } else {
                dq[&(u.clone(), w.clone())]
                    - (a_of(&a, &v) * a_of(&a, w) + a_of(&a, w) * a_of(&a, &v))
            };
            dq.insert((v.clone(), w.clone()), value);
            dq.insert((w.clone(), v.clone()), value);
        }
        let u_cols: Vec<String> = dq
            .keys()
            .filter(|(r, _)| r == &u)
            .map(|(_, c)| c.clone())
            .collect();
        for w in u_cols {
            dq.remove(&(w, u.clone()));
        }
        dq.retain(|(r, _), _| r != &u);
        *a.entry(v.clone()).or_insert(0.0) += a_of(&a, &u);
        a.insert(u.clone(), 0.0);
    }
    // networkx 两条出路（提前停 / StopIteration）最终都 sorted(key=len, reverse=True) 稳定排序。
    let mut ordered_keys = key_order;
    ordered_keys.sort_by_key(|key| std::cmp::Reverse(members.get(key).map_or(0, BTreeSet::len)));
    let mut result = Vec::new();
    for (index, key) in ordered_keys.iter().enumerate() {
        let members_set = &members[key];
        let mut viewers: Vec<String> = members_set
            .iter()
            .filter(|n| n.starts_with("viewer:"))
            .map(|n| n["viewer:".len()..].to_string())
            .collect();
        viewers.sort();
        let mut entities: Vec<String> = members_set
            .iter()
            .filter(|n| type_lookup.get(*n).map(String::as_str) == Some("Entity"))
            .cloned()
            .collect();
        entities.sort();
        if (viewers.len() as i64) < minimum_size {
            continue;
        }
        result.push(json!({
            "community_id": format!("community:{}", index + 1),
            "viewer_ids": viewers,
            "entity_ids": entities,
            "entities": entities.iter().map(|n| name_lookup.get(n).cloned().unwrap_or_else(|| n.clone())).collect::<Vec<_>>(),
            "member_count": members_set.len() as i64,
        }));
    }
    // 过滤后 community_id 必须重排（Python enumerate 在过滤前编号——
    // 读码复核：enumerate(communities, 1) 对过滤前的列表编号，过滤后编号有缺口。
    // golden 对账以 fixture 为准；此处对齐 Python 的「先编号后过滤」语义）。
    result
}

// ---------------------------------------------------------------------------
// project（移植 repo.project 三臂 + stats + 状态拼装）
// ---------------------------------------------------------------------------

fn node_item(mut row: Map<String, Value>) -> Value {
    parse_json_properties(&mut row);
    rename_key(&mut row, "node_id", "id");
    rename_key(&mut row, "node_type", "type");
    Value::Object(row)
}

fn parse_json_properties(row: &mut Map<String, Value>) {
    if let Some(Value::String(text)) = row.remove("properties_json") {
        row.insert(
            "properties".to_string(),
            serde_json::from_str(&text).unwrap_or(Value::Null),
        );
    }
}

fn rename_key(row: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = row.remove(from) {
        row.insert(to.to_string(), value);
    }
}

/// Python `repo.project`：受控只读导出；SQL 三臂逐字。
pub fn project(store: &Store, options: &ProjectOptions) -> Result<Value> {
    let (node_sql, node_params) = if !options.include_situation_actions {
        (
            "SELECT * FROM nodes WHERE node_type NOT IN ('Situation','Action') ORDER BY node_type,name",
            Vec::new(),
        )
    } else if let Some(situation_run) = &options.situation_run_id {
        (
            "SELECT * FROM nodes n WHERE n.node_type NOT IN ('Situation','Action') OR EXISTS (\
               SELECT 1 FROM edges e WHERE e.source_id=n.node_id AND e.valid_to IS NULL AND e.run_id=?\
             ) ORDER BY node_type,name",
            vec![sql_arg(Some(situation_run))],
        )
    } else {
        ("SELECT * FROM nodes ORDER BY node_type,name", Vec::new())
    };
    let (edge_sql, edge_params) = if !options.include_situation_actions {
        (
            "SELECT * FROM edges e WHERE valid_to IS NULL \
               AND (? IS NULL OR e.source_kind NOT IN ('ai_state','ai_semantic') OR e.run_id=?) \
               AND NOT EXISTS (SELECT 1 FROM nodes n WHERE n.node_id IN (e.source_id,e.target_id) \
                 AND n.node_type IN ('Situation','Action')) \
             ORDER BY predicate,source_id,target_id",
            vec![
                sql_arg(options.current_run_id.as_deref()),
                sql_arg(options.current_run_id.as_deref()),
            ],
        )
    } else if let Some(situation_run) = &options.situation_run_id {
        (
            "SELECT * FROM edges e WHERE valid_to IS NULL \
               AND (? IS NULL OR e.source_kind NOT IN ('ai_state','ai_semantic') OR e.run_id=?) \
               AND NOT EXISTS (SELECT 1 FROM nodes n WHERE n.node_id IN (e.source_id,e.target_id) \
                 AND n.node_type IN ('Situation','Action') AND e.run_id<>?) \
             ORDER BY predicate,source_id,target_id",
            vec![
                sql_arg(options.current_run_id.as_deref()),
                sql_arg(options.current_run_id.as_deref()),
                sql_arg(Some(situation_run)),
            ],
        )
    } else {
        (
            "SELECT * FROM edges e WHERE valid_to IS NULL \
               AND (? IS NULL OR e.source_kind NOT IN ('ai_state','ai_semantic') OR e.run_id=?) \
             ORDER BY predicate,source_id,target_id",
            vec![
                sql_arg(options.current_run_id.as_deref()),
                sql_arg(options.current_run_id.as_deref()),
            ],
        )
    };
    let nodes: Vec<Value> = query::fetch_all(store, node_sql, node_params, options.limit)?
        .into_iter()
        .map(node_item)
        .collect();
    let edges: Vec<Value> = query::fetch_all(store, edge_sql, edge_params, options.limit)?
        .into_iter()
        .map(|mut row| {
            // v6 去范式列只在 Rust 内部服务行级判定；parity 导出面剥除（v5 无此列）。
            row.remove("viewer_id");
            rename_key(&mut row, "edge_id", "id");
            rename_key(&mut row, "source_id", "source");
            rename_key(&mut row, "target_id", "target");
            parse_json_properties(&mut row);
            if let Some(Value::String(text)) = row.remove("evidence_json") {
                row.insert(
                    "evidence_ids".to_string(),
                    serde_json::from_str(&text).unwrap_or(Value::Null),
                );
            }
            Value::Object(row)
        })
        .collect();
    let mentions_rows = query::fetch_all(
        store,
        "SELECT m.*,r.target_id AS entity_id, \
           json_extract(r.properties_json, '$.decision') AS decision, \
           e.canonical_name,e.entity_type \
         FROM mentions m \
         LEFT JOIN edges r ON r.source_id=m.mention_id \
           AND r.predicate='REFERS_TO' AND r.source_kind='grounded_ai' AND r.valid_to IS NULL \
         LEFT JOIN entities e ON e.entity_id=r.target_id \
         ORDER BY m.created_at DESC",
        Vec::new(),
        options.limit,
    )?;
    let mentions: Vec<Value> = mentions_rows.into_iter().map(Value::Object).collect();
    let communities = detect_communities(&nodes, &edges, options.minimum_community_size);
    let stats = json!({
        "nodes": nodes.len() as i64,
        "edges": edges.len() as i64,
        "episodes": store.count_scalar("SELECT COUNT(*) FROM episodes", &[])?,
        "mentions": store.count_scalar("SELECT COUNT(*) FROM mentions", &[])?,
        "entities": store.count_scalar("SELECT COUNT(*) FROM nodes WHERE node_type='Entity'", &[])?,
        "interest_states": store.count_scalar(
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' \
             AND source_kind='ai_state' AND valid_to IS NULL AND (? IS NULL OR run_id=?)",
            &[sql_arg(options.current_run_id.as_deref()), sql_arg(options.current_run_id.as_deref())],
        )?,
        "communities": communities.len() as i64,
    });
    let mut graph = json!({
        "schema_version": GRAPH_SCHEMA_VERSION,
        "generated_at": store.now(),
        "stats": stats,
        "nodes": nodes,
        "edges": edges,
        "mentions": mentions,
        "communities": communities,
    });
    if options.include_episodes {
        graph["episodes"] = json!(query::episodes(store, "", options.limit)?);
    }
    if options.include_interest_states {
        let state_rows = query::fetch_all(
            store,
            "SELECT s.*,e.canonical_name,e.entity_type FROM edges s \
             JOIN entities e ON e.entity_id=s.target_id \
             WHERE s.predicate='INTERESTED_IN' AND s.source_kind='ai_state' AND s.valid_to IS NULL \
               AND (? IS NULL OR s.run_id=?) \
             ORDER BY s.source_id,s.confidence DESC",
            vec![
                sql_arg(options.current_run_id.as_deref()),
                sql_arg(options.current_run_id.as_deref()),
            ],
            options.limit,
        )?;
        let states: Vec<Value> = state_rows
            .into_iter()
            .map(|mut row| {
                row.remove("viewer_id"); // 同 edges 导出面剥除
                let properties = row
                    .remove("properties_json")
                    .and_then(|v| serde_json::from_str::<Value>(v.as_str().unwrap_or("{}")).ok())
                    .unwrap_or(json!({}));
                if let Some(Value::String(text)) = row.remove("evidence_json") {
                    row.insert(
                        "evidence_mention_ids".to_string(),
                        serde_json::from_str(&text).unwrap_or(Value::Null),
                    );
                }
                rename_key(&mut row, "edge_id", "state_id");
                let viewer_raw = row.remove("source_id").unwrap_or(Value::Null);
                let viewer_raw = viewer_raw.as_str().unwrap_or("");
                row.insert(
                    "viewer_id".to_string(),
                    json!(viewer_raw.strip_prefix("viewer:").unwrap_or(viewer_raw)),
                );
                rename_key(&mut row, "target_id", "entity_id");
                if let Value::Object(extra) = properties {
                    for (key, value) in extra {
                        row.insert(key, value);
                    }
                }
                Value::Object(row)
            })
            .collect();
        graph["interest_states"] = json!(states);
    }
    Ok(graph)
}
