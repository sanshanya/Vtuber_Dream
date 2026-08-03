//! 事实基线整理（移植 `episodes.py` `_source_statuses/_viewer_input/build_factual_baseline`
//! 与 `ai_data.py` `viewer_context`）：collector 产物 → 喂给 agent 的瞬时输入包。

use std::path::Path;

use serde_json::{Map, Value, json};

use super::evidence::viewer_evidence;
use super::py_str;
use crate::storage;

#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    /// Python `ValueError(f"collection is not complete (status={status})")`。
    #[error("collection is not complete (status={0})")]
    CollectionNotComplete(String),
    /// Python `ValueError("no viewer files found; run collect first")`。
    #[error("no viewer files found; run collect first")]
    NoViewers,
    #[error("{0}")]
    Storage(String),
}

/// Python `str(value or "missing")`：空串/Null/0/False 也按 falsy 落回 "missing"。
fn or_missing(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(v @ (Value::Number(_) | Value::Bool(true))) => py_str(v),
        _ => "missing".to_string(),
    }
}

/// Python `_source_statuses`：九个固定源槽位，键序逐字。
pub fn source_statuses(viewer: &Value) -> Value {
    let sources = viewer.get("sources").and_then(Value::as_object);
    let mut out = Map::new();
    for name in [
        "profile",
        "followings",
        "videos",
        "dynamics",
        "favorites",
        "bangumi",
        "games",
        "coins",
        "likes",
    ] {
        let row = sources
            .and_then(|map| map.get(name))
            .filter(|v| v.is_object());
        let status = or_missing(row.and_then(|v| v.get("status")));
        // Python `int(value.get("count") or 0)`：falsy（Null/False/空串/0）→ 0。
        let count = row
            .and_then(|v| v.get("count"))
            .filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
            .filter(|v| v.as_str() != Some(""))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let detail = row
            .map(|v| py_str(v.get("detail").unwrap_or(&Value::Null)))
            .unwrap_or_default();
        out.insert(
            name.to_string(),
            json!({"status": status, "count": count, "detail": detail}),
        );
    }
    Value::Object(out)
}

fn dict_or_empty(value: Option<&Value>) -> Value {
    match value {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => json!({}),
    }
}

/// Python `_viewer_input`：瞬时 viewer 档案（evidence 现算 + 源槽位 + raw 计数）。
pub fn viewer_input(viewer: &Value, max_evidence_per_viewer: usize) -> Value {
    let evidence = viewer_evidence(viewer, max_evidence_per_viewer);
    json!({
        "viewer": dict_or_empty(viewer.get("viewer")),
        "profile": dict_or_empty(viewer.get("profile")),
        "source_statuses": source_statuses(viewer),
        "raw_item_count": evidence.len() as i64,
        "evidence_catalog": Value::Array(evidence),
    })
}

/// Python `build_factual_baseline`：collection 门禁 → viewers → profiles（按 id 升序）+ 汇总。
pub fn build_factual_baseline(
    root: &Path,
    max_evidence_per_view: usize,
) -> Result<Value, BaselineError> {
    let collection =
        storage::read_json(&root.join("collection.json")).map_err(BaselineError::Storage)?;
    let collection = match collection {
        Some(value)
            if !matches!(
                value.get("status").and_then(Value::as_str),
                Some("complete")
            ) =>
        {
            // Python: status = collection.get("status") if isinstance(dict) else "missing"
            // f-string 下 None → "None"。
            let status = if value.is_object() {
                match value.get("status") {
                    Some(raw) if !raw.is_null() => py_str(raw),
                    _ => "None".to_string(),
                }
            } else {
                "missing".to_string()
            };
            return Err(BaselineError::CollectionNotComplete(status));
        }
        Some(value) => value,
        None => return Err(BaselineError::CollectionNotComplete("missing".to_string())),
    };
    let viewers = storage::load_viewers(root).map_err(BaselineError::Storage)?;
    if viewers.is_empty() {
        return Err(BaselineError::NoViewers);
    }
    let mut profiles: Vec<Value> = viewers
        .iter()
        .map(|viewer| viewer_input(viewer, max_evidence_per_view))
        .collect();
    // Python: profiles.sort(key=str(viewer.id or "")) — 稳定升序。
    profiles.sort_by(|left, right| {
        py_str(left["viewer"].get("id").unwrap_or(&Value::Null))
            .cmp(&py_str(right["viewer"].get("id").unwrap_or(&Value::Null)))
    });
    let raw_item_count: i64 = profiles
        .iter()
        .filter_map(|profile| profile["raw_item_count"].as_i64())
        .sum();
    let viewers_with_public_evidence = profiles
        .iter()
        .filter(|profile| profile["raw_item_count"].as_i64().unwrap_or(0) > 0)
        .count() as i64;
    Ok(json!({
        "summary": {
            "viewer_count": viewers.len() as i64,
            "raw_item_count": raw_item_count,
            "viewers_with_public_evidence": viewers_with_public_evidence,
            "collection_request_count": collection.get("request_count").and_then(Value::as_i64).unwrap_or(0),
            "collection_elapsed_seconds": collection.get("elapsed_seconds").and_then(Value::as_f64).unwrap_or(0.0),
        },
        "viewer_profiles": profiles,
        "streamer": storage::read_json(&root.join("streamer.json"))
            .map_err(BaselineError::Storage)?
            .unwrap_or(json!({})),
        "platform_snapshot": storage::read_json(&root.join("shared/platform_snapshot.json"))
            .map_err(BaselineError::Storage)?
            .unwrap_or(json!({})),
    }))
}

/// Python `viewer_context`：baseline.evidence_catalog 为列表时直接复用，否则现算。
pub fn viewer_context(viewer: &Value, baseline: &Value, max_evidence_per_viewer: usize) -> Value {
    let evidence = match baseline.get("evidence_catalog") {
        Some(v @ Value::Array(_)) => v.clone(),
        _ => Value::Array(viewer_evidence(viewer, max_evidence_per_viewer)),
    };
    json!({
        "viewer": baseline.get("viewer").cloned().unwrap_or(json!({})),
        "public_profile": baseline.get("profile").cloned().unwrap_or(json!({})),
        "source_statuses": baseline.get("source_statuses").cloned().unwrap_or(json!({})),
        "evidence": evidence,
        "collected_at": py_str(viewer.get("collected_at").unwrap_or(&Value::Null)),
    })
}
