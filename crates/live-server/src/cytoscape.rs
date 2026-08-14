//! G2：project() JSON → cytoscape elements 转换层（新增，不扰 project 契约——
//! M4 钉团把 project 形状锁死，这里是薄 DTO 适配）。
//!
//! 元素形态（前端 style 预设消费）：
//! - 节点：`{data:{id, label, kind, properties}}`
//! - 边：`{data:{id, source, target, predicate, confidence, evidence_ids, properties}}`

use serde_json::{Value, json};

/// project() 节点→cytoscape 元素（拾 id/name/type/properties 四键）。
fn node_element(node: &Value) -> Value {
    json!({
        "data": {
            "id": node["id"],
            "label": node["name"],
            "kind": node["type"],
            "properties": node["properties"],
        }
    })
}

fn edge_element(edge: &Value) -> Value {
    json!({
        "data": {
            "id": edge["id"],
            "source": edge["source"],
            "target": edge["target"],
            "predicate": edge["predicate"],
            "confidence": edge["confidence"],
            "evidence_ids": edge["evidence_ids"],
            "properties": edge["properties"],
        }
    })
}

/// DTO 增量（删码刀13 前置）：节点 data 增补 `degree` 与 `community_id`——
/// degree = **出图边集**内的邻居计数（折叠/局部各随其边集，语义=所见图内的度，
/// 面板 LOD/尺寸分档共用此口径）；community_id = project() 已算的 CNM 社区
/// （viewer_ids∪entity_ids 反查——此前算了白算，从未送出聚合层）。
fn enrich(mut elements: Value, project: &Value) -> Value {
    let mut degrees: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    if let Some(arr) = elements["elements"].as_array() {
        for el in arr {
            if el["data"]["source"].is_string() {
                for key in ["source", "target"] {
                    if let Some(id) = el["data"][key].as_str() {
                        *degrees.entry(id.to_string()).or_default() += 1;
                    }
                }
            }
        }
    }
    let mut communities: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Some(arr) = project["communities"].as_array() {
        for community in arr {
            let Some(id) = community["community_id"].as_str() else {
                continue;
            };
            for key in ["viewer_ids", "entity_ids"] {
                if let Some(members) = community[key].as_array() {
                    for member in members.iter().filter_map(Value::as_str) {
                        communities.insert(member.to_string(), id.to_string());
                    }
                }
            }
        }
    }
    if let Some(arr) = elements["elements"].as_array_mut() {
        for el in arr {
            if el["data"]["source"].is_string() {
                continue;
            }
            let Some(id) = el["data"]["id"].as_str().map(str::to_string) else {
                continue;
            };
            el["data"]["degree"] = json!(degrees.get(&id).copied().unwrap_or(0));
            if let Some(community) = communities.get(&id) {
                el["data"]["community_id"] = json!(community);
            }
        }
    }
    elements
}

/// 整体图形态（get 全量 project() 出口）。
pub fn elements(project: &Value) -> Value {
    let mut elements: Vec<Value> = Vec::new();
    if let Some(nodes) = project["nodes"].as_array() {
        elements.extend(nodes.iter().map(node_element));
    }
    if let Some(edges) = project["edges"].as_array() {
        elements.extend(edges.iter().map(edge_element));
    }
    enrich(json!({ "elements": elements }), project)
}

/// kind 折叠投影——只保留 expanded 类节点；悬空边（任一端不存活）整边裁除。
/// 节点序保持 project ORDER BY 位序；边序同理。未知 kind 的节点一律视为「非展开」
/// （白名单是收口面，不是增幅面——不让未知类型借折叠缝流进默认视图）。
/// 2026-08-05 用户裁决：折叠面上**零存活边节点一律不出图**（生产实测 90% 是散点噪声
/// 且为前端 3GB 内存主凶）——这是视图裁剪，图层数据照旧全量，可回放可钻取。
pub fn elements_expanded(project: &Value, expanded: &std::collections::BTreeSet<String>) -> Value {
    let mut kept_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut node_elements: Vec<Value> = Vec::new();
    if let Some(nodes) = project["nodes"].as_array() {
        for node in nodes {
            let kind = node["type"].as_str().unwrap_or("");
            let id = node["id"].as_str().unwrap_or("");
            if !expanded.contains(kind) || id.is_empty() {
                continue;
            }
            kept_ids.insert(id.to_string());
            node_elements.push(node_element(node));
        }
    }
    let mut connected: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut elements: Vec<Value> = Vec::new();
    if let Some(edges) = project["edges"].as_array() {
        for edge in edges {
            let source = edge["source"].as_str().unwrap_or("");
            let target = edge["target"].as_str().unwrap_or("");
            if !kept_ids.contains(source) || !kept_ids.contains(target) {
                continue;
            }
            connected.insert(source.to_string());
            connected.insert(target.to_string());
            elements.push(edge_element(edge));
        }
    }
    // 零度裁决：只吐出出现在存活边端点上的节点（节点序仍保持项目位序）。
    let nodes: Vec<Value> = node_elements
        .into_iter()
        .filter(|element| {
            element["data"]["id"]
                .as_str()
                .is_some_and(|id| connected.contains(id))
        })
        .collect();
    elements.splice(0..0, nodes);
    enrich(json!({ "elements": elements }), project)
}

/// 局部图（个人页）：与 viewer 节点相邻的边 + 其两端节点。
pub fn scoped(project: &Value, node_id: &str) -> Value {
    let mut node_ids: std::collections::BTreeSet<String> = [node_id.to_string()].into();
    let mut elements: Vec<Value> = Vec::new();
    if let Some(edges) = project["edges"].as_array() {
        for edge in edges {
            let adjacent = edge["source"].as_str() == Some(node_id)
                || edge["target"].as_str() == Some(node_id);
            if !adjacent {
                continue;
            }
            for key in ["source", "target"] {
                if let Some(id) = edge[key].as_str() {
                    node_ids.insert(id.to_string());
                }
            }
            elements.push(edge_element(edge));
        }
    }
    if let Some(nodes) = project["nodes"].as_array() {
        for node in nodes.iter().rev() {
            if node["id"].as_str().is_some_and(|id| node_ids.contains(id)) {
                elements.insert(0, node_element(node));
            }
        }
    }
    enrich(json!({ "elements": elements }), project)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mini_project() -> Value {
        json!({
            "nodes": [
                {"id": "viewer:1", "name": "甲", "type": "Viewer", "properties": {"viewer_id": "1"}},
                {"id": "entity:原神", "name": "原神", "type": "Entity", "properties": {}},
                {"id": "episode:e1", "name": "场1", "type": "Episode", "properties": {}},
                {"id": "mention:m1", "name": "m1", "type": "Mention", "properties": {}}
            ],
            "edges": [
                {"id": "r1", "source": "viewer:1", "target": "entity:原神", "predicate": "INTERESTED_IN",
                 "confidence": 0.8, "evidence_ids": ["m1"], "properties": {}},
                {"id": "r2", "source": "episode:e1", "target": "mention:m1", "predicate": "CONTAINS_MENTION"},
                {"id": "r3", "source": "viewer:1", "target": "mention:m1", "predicate": "REFERS_TO"},
                {"id": "r4", "source": "entity:原神", "target": "viewer:1", "predicate": "FOLLOWED_BY"}
            ]
        })
    }

    #[test]
    fn elements_unfiltered_keeps_everything() {
        let out = elements(&mini_project());
        assert_eq!(out["elements"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn elements_expanded_folds_kinds_and_drops_dangling_edges() {
        let expanded: std::collections::BTreeSet<String> =
            ["Viewer".to_string(), "Entity".to_string()]
                .into_iter()
                .collect();
        let out = elements_expanded(&mini_project(), &expanded);
        let list = out["elements"].as_array().unwrap();
        let nodes: Vec<&Value> = list
            .iter()
            .filter(|el| el["data"]["kind"].is_string())
            .collect();
        let edges: Vec<&Value> = list
            .iter()
            .filter(|el| !el["data"]["source"].is_null())
            .collect();
        assert_eq!(nodes.len(), 2, "Viewer+Entity 存活：{out}");
        // r1（双端存活）+ r4（双端存活）在；r2（双端均折叠）、r3（target 折叠）裁除。
        let edge_ids: Vec<&str> = edges
            .iter()
            .map(|el| el["data"]["id"].as_str().unwrap())
            .collect();
        assert_eq!(edge_ids, ["r1", "r4"], "{out}");
    }

    #[test]
    fn elements_expanded_empty_whitelist_yields_no_edges() {
        let expanded: std::collections::BTreeSet<String> = Default::default();
        let out = elements_expanded(&mini_project(), &expanded);
        assert_eq!(out["elements"].as_array().unwrap().len(), 0);
    }

    /// 2026-08-05 用户裁决：生产折叠视图 2522 节点中 2271 个（90%）是零存活边散点——
    /// 满屏紫点=零信息量，且是 3GB 内存的主凶之一。「仅人与关注点主骨架」必须
    /// 落纸：折叠面上零度节点（含 Viewer——视图裁剪不丢数据，图层照旧全量）一律不出。
    #[test]
    fn elements_expanded_drops_zero_degree_nodes_in_folded_view() {
        let mut project = mini_project();
        project["nodes"].as_array_mut().unwrap().extend([
            json!({"id": "entity:孤岛", "name": "孤岛", "type": "Entity", "properties": {}}),
            json!({"id": "viewer:9", "name": "孤观众", "type": "Viewer", "properties": {"viewer_id": "9"}}),
        ]);
        let expanded: std::collections::BTreeSet<String> =
            ["Viewer".to_string(), "Entity".to_string()]
                .into_iter()
                .collect();
        let out = elements_expanded(&project, &expanded);
        let list = out["elements"].as_array().unwrap();
        let node_ids: Vec<&str> = list
            .iter()
            .filter(|el| el["data"]["kind"].is_string())
            .map(|el| el["data"]["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            node_ids,
            ["viewer:1", "entity:原神"],
            "零度节点一律不出图: {out}"
        );
        let edge_ids: Vec<&str> = list
            .iter()
            .filter(|el| !el["data"]["source"].is_null())
            .map(|el| el["data"]["id"].as_str().unwrap())
            .collect();
        assert_eq!(edge_ids, ["r1", "r4"], "{out}");
    }

    /// enrich 钉（删码刀13 前置）：degree = 出图边集内邻居计数（折叠/局部各随其
    /// 边集）；community_id = project 已算 CNM 反穿（此前算了白算未出聚合层）。
    #[test]
    fn enrich_stamps_degree_and_community() {
        let mut project = mini_project();
        project["communities"] = json!([{
            "community_id": "community:2",
            "viewer_ids": ["viewer:1"],
            "entity_ids": ["entity:原神"],
            "entities": ["原神"],
            "member_count": 2,
        }]);
        // 全量面：viewer:1 三边（INTERESTED_IN/REFERS_TO/FOLLOWED_BY）→ degree=3；
        // entity:原神 双边；mention:m1 双边；episode 单边。
        let out = elements(&project);
        let find = |id: &str| {
            out["elements"]
                .as_array()
                .unwrap()
                .iter()
                .find(|el| el["data"]["id"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("{id}"))
        };
        assert_eq!(find("viewer:1")["data"]["degree"], 3);
        assert_eq!(find("entity:原神")["data"]["degree"], 2);
        assert_eq!(find("episode:e1")["data"]["degree"], 1);
        assert_eq!(find("viewer:1")["data"]["community_id"], "community:2");
        assert_eq!(find("entity:原神")["data"]["community_id"], "community:2");
        // 无社区归属者：键缺席（前端「未编组」三态——绝不臆造 community:0）。
        assert!(find("episode:e1")["data"].get("community_id").is_none());
        // 局部图（viewer:1 邻域）：episode:e1 那条 CONTAINS_MENTION 出圈，
        // 其余三边在圈——degree 语义=所见图内的度。
        let scoped_out = scoped(&project, "viewer:1");
        let find_s = |id: &str| {
            scoped_out["elements"]
                .as_array()
                .unwrap()
                .iter()
                .find(|el| el["data"]["id"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("{id}"))
        };
        assert_eq!(find_s("viewer:1")["data"]["degree"], 3);
    }
}
