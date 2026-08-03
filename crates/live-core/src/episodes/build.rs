//! Episode 构建（移植 `episodes.py`）：evidence → 不可变 Episode（含 id 指纹 + 字段集）。

use super::evidence::source_text;
use super::*;

fn episode_id(viewer_id: &str, evidence: &Value, content_version: &str) -> String {
    let raw_id = py_str(get_value(evidence, "id"));
    let stable = if raw_id.is_empty() {
        hash_parts(
            &[
                py_str(get_value(evidence, "source")),
                py_str(get_value(evidence, "bvid")),
                py_str(get_value(evidence, "title")),
                py_str(get_value(evidence, "url")),
            ],
            20,
        )
    } else {
        raw_id
    };
    format!("episode:{viewer_id}:{stable}:{content_version}")
}

fn make_field(path: &str, value: &Value, kind: &str, limit: usize) -> Option<EpisodeField> {
    let text = source_text(value, limit);
    if text.is_empty() {
        None
    } else {
        Some(EpisodeField {
            path: path.to_string(),
            text,
            kind: kind.to_string(),
        })
    }
}

/// `evidence_to_episode` 移植：fields 顺序、platform_facts 形态、content_version 公式
/// 全部与 Python 逐字节一致。
pub fn evidence_to_episode(
    viewer_id: &str,
    evidence: &Value,
    observed_at: Option<&str>,
) -> Episode {
    let mut fields: Vec<EpisodeField> = Vec::new();
    for item in [
        make_field("title", get_value(evidence, "title"), "text", 4_000),
        make_field(
            "description",
            get_value(evidence, "description"),
            "text",
            20_000,
        ),
        make_field(
            "creator_name",
            get_value(evidence, "creator_name"),
            "platform_creator",
            2_000,
        ),
        make_field(
            "folder_name",
            get_value(evidence, "folder_name"),
            "platform_container",
            2_000,
        ),
    ]
    .into_iter()
    .flatten()
    {
        fields.push(item);
    }

    let tags: Vec<Value> = match get_value(evidence, "tags") {
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    for (index, tag) in tags.iter().enumerate() {
        if let Some(field) = make_field(&format!("tags[{index}]"), tag, "platform_tag", 2_000) {
            fields.push(field);
        }
    }

    let category = match get_value(evidence, "platform_category") {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    for key in ["name", "v2_name"] {
        if let Some(field) = make_field(
            &format!("platform_category.{key}"),
            category.get(key).unwrap_or(&Value::Null),
            "platform_category",
            2_000,
        ) {
            fields.push(field);
        }
    }

    let tag_strings: Vec<Value> = tags
        .iter()
        .map(py_str)
        .filter(|item| !item.trim().is_empty())
        .map(Value::String)
        .collect();
    let mut platform_facts = Map::new();
    platform_facts.insert(
        "evidence_id".to_string(),
        Value::String(py_str(get_value(evidence, "id"))),
    );
    platform_facts.insert(
        "creator_id".to_string(),
        Value::String(py_str(get_value(evidence, "creator_id"))),
    );
    platform_facts.insert(
        "creator_name".to_string(),
        Value::String(py_str(get_value(evidence, "creator_name"))),
    );
    platform_facts.insert(
        "folder_name".to_string(),
        Value::String(py_str(get_value(evidence, "folder_name"))),
    );
    platform_facts.insert("tags".to_string(), Value::Array(tag_strings));
    platform_facts.insert("platform_category".to_string(), Value::Object(category));
    platform_facts.insert(
        "source_label".to_string(),
        Value::String(py_str(get_value(evidence, "source_label"))),
    );

    let raw_source = py_str(get_value(evidence, "source"));
    // Python parity：source 回退为 "unknown"，event_type 独立回退为 "observation"。
    let source = if raw_source.is_empty() {
        "unknown".to_string()
    } else {
        raw_source.clone()
    };
    let event_type = format!(
        "public_{}",
        if raw_source.is_empty() {
            "observation"
        } else {
            raw_source.as_str()
        }
    );
    let published_at = py_str(get_value(evidence, "published_at"));
    let title = source_text(get_value(evidence, "title"), 4_000);
    let url = source_text(get_value(evidence, "url"), 4_000);
    let bvid = source_text(get_value(evidence, "bvid"), 200);

    let mut version_doc = Map::new();
    version_doc.insert("source".to_string(), Value::String(source.clone()));
    version_doc.insert("event_type".to_string(), Value::String(event_type.clone()));
    version_doc.insert(
        "published_at".to_string(),
        Value::String(published_at.clone()),
    );
    version_doc.insert("title".to_string(), Value::String(title.clone()));
    version_doc.insert("url".to_string(), Value::String(url.clone()));
    version_doc.insert("bvid".to_string(), Value::String(bvid.clone()));
    version_doc.insert(
        "fields".to_string(),
        Value::Array(
            fields
                .iter()
                .map(|field| {
                    let mut item = Map::new();
                    item.insert("path".to_string(), Value::String(field.path.clone()));
                    item.insert("text".to_string(), Value::String(field.text.clone()));
                    item.insert("kind".to_string(), Value::String(field.kind.clone()));
                    Value::Object(item)
                })
                .collect(),
        ),
    );
    let facts_value = Value::Object(platform_facts);
    version_doc.insert("platform_facts".to_string(), facts_value.clone());
    let content_version = hash_parts(&[json_canon(&Value::Object(version_doc))], 16);

    Episode {
        episode_id: episode_id(viewer_id, evidence, &content_version),
        viewer_id: viewer_id.to_string(),
        source,
        event_type,
        observed_at: observed_at.map(str::to_string).unwrap_or_else(now_iso),
        published_at,
        title,
        url,
        bvid,
        fields,
        platform_facts: facts_value,
    }
}

/// `build_viewer_episodes`：viewer 文件 → Episode 列表。
pub fn build_viewer_episodes(viewer: &Value, max_evidence_per_viewer: usize) -> Vec<Episode> {
    // 便捷组合：raw → viewer_evidence → episodes_from_context。
    // 等价 Python `build_viewer_episodes(viewer_context(raw, 无 catalog 的 baseline))`。
    episodes_from_context(&serde_json::json!({
        "viewer": get_value(viewer, "viewer"),
        "collected_at": py_str(get_value(viewer, "collected_at")),
        "evidence": viewer_evidence(viewer, max_evidence_per_viewer),
    }))
}

/// Python `episodes.build_viewer_episodes(context)` 真平移：消费 context["evidence"]
/// （不现算——上游 catalog 工程化裁剪必须被尊重；M4-A bundle 对账钉）。
pub fn episodes_from_context(context: &Value) -> Vec<Episode> {
    let uid = py_str(get_value(get_value(context, "viewer"), "id"));
    let collected_at = {
        let text = py_str(get_value(context, "collected_at"));
        if text.is_empty() { None } else { Some(text) }
    };
    get_value(context, "evidence")
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item.is_object())
                .map(|evidence| evidence_to_episode(&uid, evidence, collected_at.as_deref()))
                .collect()
        })
        .unwrap_or_default()
}
