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

/// 整体图形态（get 全量 project() 出口）。
pub fn elements(project: &Value) -> Value {
    let mut elements: Vec<Value> = Vec::new();
    if let Some(nodes) = project["nodes"].as_array() {
        elements.extend(nodes.iter().map(node_element));
    }
    if let Some(edges) = project["edges"].as_array() {
        elements.extend(edges.iter().map(edge_element));
    }
    json!({ "elements": elements })
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
    json!({ "elements": elements })
}
