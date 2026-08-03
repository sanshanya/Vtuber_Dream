//! 观众证据置形（移植 `ai_data.viewer_evidence`）：profile 先行 + 固定顺序六源，
//! 按 (published_at_ts, source) 稳定降序，id 去重，max 封顶。

use super::get_value;
use super::*;

// ---------------------------------------------------------------------------
// 观众证据（ai_data.viewer_evidence）
// ---------------------------------------------------------------------------

/// source → 展示标签（SOURCE_LABELS 七项）。
pub const SOURCE_LABELS: [(&str, &str); 7] = [
    ("profile", "个人简介"),
    ("following", "公开关注"),
    ("video", "本人投稿"),
    ("dynamic", "公开动态"),
    ("favorite", "公开收藏"),
    ("bangumi", "公开追番"),
    ("game", "最近游戏"),
];

pub fn source_label(source_type: &str, fallback: &str) -> String {
    SOURCE_LABELS
        .iter()
        .find(|(key, _)| *key == source_type)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| fallback.to_string())
}

/// `source_text(value, limit)` = str(value or "").strip()[:limit]
pub fn source_text(value: &Value, limit: usize) -> String {
    char_prefix(py_str(value).trim(), limit)
}

/// `source_string_list`：list[str]，元素 source_text(4000)，去空去重保序。
pub fn source_string_list(value: &Value) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    if let Value::Array(items) = value {
        for item in items {
            let text = source_text(item, 4_000);
            if !text.is_empty() && !result.contains(&text) {
                result.push(text);
            }
        }
    }
    result
}

/// `_evidence_id(uid, item)` = sha1("uid|source|id|bvid|title|url")[:16]
pub fn evidence_id(
    uid: &str,
    source: &str,
    id: &str,
    bvid: &str,
    title: &str,
    url: &str,
) -> String {
    hash_parts(
        &[
            uid.to_string(),
            source.to_string(),
            id.to_string(),
            bvid.to_string(),
            title.to_string(),
            url.to_string(),
        ],
        16,
    )
}

/// `_parse_time`：先按 float 解析（epoch 字符串），失败按 ISO8601，再失败 0.0。
fn parse_time(value: &Value) -> f64 {
    let text = py_str(value).trim().to_string();
    if text.is_empty() {
        return 0.0;
    }
    if let Ok(number) = text.parse::<f64>() {
        return number;
    }
    let normalized = text.replace('Z', "+00:00");
    match chrono::DateTime::parse_from_rfc3339(&normalized) {
        // Python datetime.timestamp() 保留小数秒；用 millis 保留亚秒精度。
        Ok(moment) => moment.timestamp_millis() as f64 / 1000.0,
        // Python fromisoformat 接受"YYYY-MM-DD HH:MM:SS"等更多形态；
        // 当前语料均为 RFC3339，分歧点注释在案（known-divergence）。
        Err(_) => 0.0,
    }
}

/// 观众的全部公开证据：profile 先行，随后固定顺序六个源；
/// 按 (published_at_ts, source) 稳定降序，id 去重，max 封顶。
/// 移植自 `ai_data.viewer_evidence`。
pub fn viewer_evidence(viewer: &Value, max_evidence_per_viewer: usize) -> Vec<Value> {
    let uid = py_str(get_value(get_value(viewer, "viewer"), "id"));
    let mut candidates: Vec<Value> = Vec::new();

    let profile = get_value(viewer, "profile");
    let profile_text = [
        source_text(get_value(profile, "sign"), 4_000),
        source_text(get_value(profile, "official_title"), 2_000),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    if !profile_text.is_empty() {
        let title = py_str(get_value(profile, "name"));
        let url = py_str(get_value(profile, "profile_url"));
        let mut item = Map::new();
        item.insert(
            "id".to_string(),
            Value::String(evidence_id(&uid, "profile", &uid, "", &title, &url)),
        );
        item.insert("source".to_string(), Value::String("profile".to_string()));
        item.insert(
            "source_label".to_string(),
            Value::String(source_label("profile", "profile")),
        );
        let fallback_title = py_str(get_value(get_value(viewer, "viewer"), "name"));
        let title = if title.is_empty() {
            fallback_title
        } else {
            title
        };
        item.insert(
            "title".to_string(),
            Value::String(char_prefix(title.trim(), 1_000)),
        );
        item.insert("description".to_string(), Value::String(profile_text));
        item.insert("creator_id".to_string(), Value::String(String::new()));
        item.insert("creator_name".to_string(), Value::String(String::new()));
        item.insert("folder_name".to_string(), Value::String(String::new()));
        item.insert("tags".to_string(), Value::Array(Vec::new()));
        item.insert("platform_category".to_string(), Value::Object(Map::new()));
        item.insert("published_at".to_string(), Value::String(String::new()));
        let fallback_url = py_str(get_value(get_value(viewer, "viewer"), "profile_url"));
        let url = if url.is_empty() { fallback_url } else { url };
        item.insert(
            "url".to_string(),
            Value::String(char_prefix(url.trim(), 2_000)),
        );
        item.insert("bvid".to_string(), Value::String(String::new()));
        candidates.push(Value::Object(item));
    }

    let sources = get_value(viewer, "sources");
    // Python: source_name.rstrip("s")
    for source_name in [
        "followings",
        "videos",
        "dynamics",
        "favorites",
        "bangumi",
        "games",
    ] {
        let source = get_value(sources, source_name);
        let items = match get_value(source, "items") {
            Value::Array(items) => items,
            _ => continue,
        };
        for raw in items {
            if !raw.is_object() {
                continue;
            }
            // Python parity: _evidence_id 哈希使用原始 raw.source / raw.title，
            // 回退值仅用于展示字段，不得进入哈希槽位。
            let raw_source = py_str(get_value(raw, "source"));
            let raw_title = py_str(get_value(raw, "title"));
            let source_type = if raw_source.is_empty() {
                source_name.trim_end_matches('s').to_string()
            } else {
                raw_source.clone()
            };
            let title_raw = if raw_title.is_empty() {
                py_str(get_value(raw, "creator_name"))
            } else {
                raw_title.clone()
            };
            let category = match get_value(raw, "platform_category") {
                Value::Object(map) => Value::Object(map.clone()),
                _ => Value::Object(Map::new()),
            };
            let mut item = Map::new();
            item.insert(
                "id".to_string(),
                Value::String(evidence_id(
                    &uid,
                    &raw_source,
                    &py_str(get_value(raw, "id")),
                    &py_str(get_value(raw, "bvid")),
                    &raw_title,
                    &py_str(get_value(raw, "url")),
                )),
            );
            item.insert("source".to_string(), Value::String(source_type.clone()));
            item.insert(
                "source_label".to_string(),
                Value::String(source_label(&source_type, source_name)),
            );
            item.insert(
                "title".to_string(),
                Value::String(char_prefix(title_raw.trim(), 4_000)),
            );
            item.insert(
                "description".to_string(),
                Value::String(source_text(get_value(raw, "description"), 20_000)),
            );
            item.insert(
                "creator_id".to_string(),
                Value::String(source_text(get_value(raw, "creator_id"), 200)),
            );
            item.insert(
                "creator_name".to_string(),
                Value::String(source_text(get_value(raw, "creator_name"), 1_000)),
            );
            item.insert(
                "folder_name".to_string(),
                Value::String(source_text(get_value(raw, "folder_name"), 1_000)),
            );
            item.insert(
                "tags".to_string(),
                Value::Array(
                    source_string_list(get_value(raw, "tags"))
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
            item.insert("platform_category".to_string(), category);
            item.insert(
                "published_at".to_string(),
                Value::String(py_str(get_value(raw, "published_at"))),
            );
            item.insert(
                "url".to_string(),
                Value::String(source_text(get_value(raw, "url"), 2_000)),
            );
            item.insert(
                "bvid".to_string(),
                Value::String(source_text(get_value(raw, "bvid"), 100)),
            );
            candidates.push(Value::Object(item));
        }
    }

    // Python sorted(..., key=tuple, reverse=True)：稳定排序的降序。
    candidates.sort_by(|left, right| {
        let lt = parse_time(get_value(left, "published_at"));
        let rt = parse_time(get_value(right, "published_at"));
        let ls = py_str(get_value(left, "source"));
        let rs = py_str(get_value(right, "source"));
        rt.partial_cmp(&lt)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| rs.cmp(&ls))
    });

    let mut selected: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in candidates {
        let identity = py_str(get_value(&item, "id"));
        if !seen.insert(identity) {
            continue;
        }
        selected.push(item);
        if selected.len() >= max_evidence_per_viewer {
            break;
        }
    }
    selected
}
