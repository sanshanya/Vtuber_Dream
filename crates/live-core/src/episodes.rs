//! Episode 与观众证据：从公开事实到不可变事实输入（移植 Python `episodes.py` + `ai_data.py`）。
//!
//! 逐字节对齐规则（黄金样本对账依赖）：
//! - `_hash(*parts, length)` = sha1("|".join(str(p or "")), utf-8).hexdigest()[:length]
//!   - Python `or ""` 语义：`None/""/0/False/[] → ""`；列表 → Python repr
//! - `_json` = ensure_ascii=False + sort_keys + separators(",", ":")
//! - `_norm` = 去除 `[\s\-_·•/\\]+` 后 casefold；Rust 用 to_lowercase 近似
//!   （已知分歧：ß/İ 等特种 casefold 不等价；当前语料为 CJK/ASCII，注释记录在案）
//! - 所有 span 偏移为字符偏移（Python str 语义），Rust 内部以 char 计数，不是字节。

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

// ---------------------------------------------------------------------------
// 基础原语
// ---------------------------------------------------------------------------

/// 当前 UTC 时间，格式与 Python `datetime.now(UTC).isoformat()` 一致
/// （六位微秒 + `+00:00` 后缀）。
pub fn now_iso() -> String {
    let now = chrono::Utc::now();
    format!(
        "{}.{:06}+00:00",
        now.format("%Y-%m-%dT%H:%M:%S"),
        now.timestamp_subsec_micros()
    )
}

/// Python `str(value or "")`：Null/空/0/False/空数组 → ""；其余 → 文本。
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => {
            if *b {
                "True".to_string()
            } else {
                String::new() // False or "" == ""
            }
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            if a.is_empty() {
                String::new()
            } else {
                // 未知路径的兜底：Python repr 的数组形态（元素走 repr_str）
                py_repr_list(
                    &a.iter()
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect::<Vec<_>>(),
                )
            }
        }
        Value::Object(_) => String::new(),
    }
}

/// Python `str(int or "")`：0 被判定为 falsy → ""。
/// 这是 hex id 对账的隐藏考点（demo-3 的 mention start=0 → 哈希里为空串）。
pub fn py_str_int(value: i64) -> String {
    if value == 0 {
        String::new()
    } else {
        value.to_string()
    }
}

/// Python `repr(str)` 的最小兼容：单引号包裹，反斜杠与单引号转义，\n\t\r 转义。
fn py_repr_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

/// Python `str(list or "")`：空列表 falsy → ""，否则 `['a', 'b']` 形态。
/// 移植锚点：`resolve_entity` 的 grounding 列表直接进 `_hash`。
pub fn py_repr_list(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let body: Vec<String> = items.iter().map(|item| py_repr_str(item)).collect();
    format!("[{}]", body.join(", "))
}

/// sha1 hex 前缀（Python `_hash`）。
pub fn hash_parts(parts: &[String], length: usize) -> String {
    let joined = parts.join("|");
    let digest = Sha1::digest(joined.as_bytes());
    let mut out = String::with_capacity(length);
    for byte in digest.iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    out.chars().take(length).collect()
}

/// 规范化 JSON 序列化：键排序、无空格、非 ASCII 原样（Python json.dumps
/// ensure_ascii=False + sort_keys + separators）。
///
/// serde_json 默认 Map 为 BTreeMap（已排序），但这里显式递归写出，
/// 保证未来开启 preserve_order 特性也不会破坏对账。
pub fn json_canon(value: &Value) -> String {
    let mut out = String::new();
    write_canon(value, &mut out);
    out
}

fn write_canon(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canon(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            for (index, (key, item)) in sorted.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                write_canon(item, out);
            }
            out.push('}');
        }
    }
}

/// 与 Python json.dumps(ensure_ascii=False) 相同的转义面：
/// \" \\ \b \f \n \r \t + <0x20 → \u00xx；非 ASCII 原样输出。
fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// `_norm`：strip + casefold（Rust 近似 to_lowercase）+ 去 `[\s\-_·•/\\]+`。
pub fn norm(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    Regex::new(r#"[\s\-_·•/\\]+"#)
        .expect("static regex")
        .replace_all(&lowered, "")
        .into_owned()
}

/// `_safe_type`：非 `0-9a-zA-Z一-鿿` 序列折叠为 `_`，去首尾 `_`，空 → "concept"。
pub fn safe_type(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let replaced = Regex::new(r#"[^0-9a-zA-Z一-鿿]+"#)
        .expect("static regex")
        .replace_all(&lowered, "_");
    let trimmed = replaced.trim_matches('_');
    if trimmed.is_empty() {
        "concept".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Python str 的字符数（span 单位）。
fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Python `text[:limit]` 的字符切片。
pub fn char_prefix(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Python `text[start:end]` 的字符切片。
fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

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

/// `clip(value, limit)` = re.sub(r"\s+", " ", str(value or "")).strip()[:limit]
pub fn clip(value: &Value, limit: usize) -> String {
    let flattened = py_str(value);
    let collapsed = Regex::new(r"\s+")
        .expect("static regex")
        .replace_all(&flattened, " ");
    char_prefix(collapsed.trim(), limit)
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

fn get_value<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
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

// ---------------------------------------------------------------------------
// Episode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeField {
    pub path: String,
    pub text: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub episode_id: String,
    pub viewer_id: String,
    pub source: String,
    pub event_type: String,
    pub observed_at: String,
    pub published_at: String,
    pub title: String,
    pub url: String,
    pub bvid: String,
    pub fields: Vec<EpisodeField>,
    pub platform_facts: Value,
}

impl Episode {
    pub fn field_text(&self, path: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.path == path)
            .map(|field| field.text.as_str())
    }
}

/// `_episode_id`：stable = evidence.id 或 sha1(source|bvid|title|url)[:20]。
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
    let uid = py_str(get_value(get_value(viewer, "viewer"), "id"));
    let collected_at = {
        let text = py_str(get_value(viewer, "collected_at"));
        if text.is_empty() { None } else { Some(text) }
    };
    viewer_evidence(viewer, max_evidence_per_viewer)
        .iter()
        .map(|evidence| evidence_to_episode(&uid, evidence, collected_at.as_deref()))
        .collect()
}

// ---------------------------------------------------------------------------
// 确定式 mention seeds 与 span 校验
// ---------------------------------------------------------------------------

fn quoted_patterns() -> Vec<Regex> {
    vec![
        Regex::new(r"《([^《》]{1,120})》").expect("static regex"),
        Regex::new(r"【([^【】]{1,120})】").expect("static regex"),
        Regex::new(r"#([^#\n]{1,80})#").expect("static regex"),
    ]
}

/// 字节偏移 → 字符偏移（regex 返回字节，Python 语义是字符）。
fn byte_to_char_offset(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset].chars().count()
}

/// `deterministic_mention_seeds`：只提取显式表面候选；语义裁决仍在 Agent。
pub fn deterministic_mention_seeds(episodes: &[Episode]) -> Vec<Value> {
    let mut seeds: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, usize, usize)> =
        std::collections::HashSet::new();

    let mut add = |episode: &Episode,
                   field: &EpisodeField,
                   start: usize,
                   end: usize,
                   text: &str,
                   kind: &str,
                   origin: &str| {
        let key = (episode.episode_id.clone(), field.path.clone(), start, end);
        if seen.contains(&key) || text.trim().is_empty() {
            return;
        }
        seen.insert(key.clone());
        let seed_id = format!(
            "seed:{}",
            hash_parts(
                &[
                    episode.episode_id.clone(),
                    field.path.clone(),
                    py_str_int(start as i64),
                    py_str_int(end as i64),
                ],
                20,
            )
        );
        let mut seed = Map::new();
        seed.insert("seed_id".to_string(), Value::String(seed_id));
        seed.insert(
            "episode_id".to_string(),
            Value::String(episode.episode_id.clone()),
        );
        seed.insert("field_path".to_string(), Value::String(field.path.clone()));
        seed.insert("text".to_string(), Value::String(text.to_string()));
        seed.insert("start".to_string(), Value::from(start as i64));
        seed.insert("end".to_string(), Value::from(end as i64));
        seed.insert("surface_kind".to_string(), Value::String(kind.to_string()));
        seed.insert("origin".to_string(), Value::String(origin.to_string()));
        seeds.push(Value::Object(seed));
    };

    for episode in episodes {
        for field in &episode.fields {
            if field.kind.starts_with("platform_") {
                add(
                    episode,
                    field,
                    0,
                    char_len(&field.text),
                    &field.text.clone(),
                    &field.kind,
                    "platform",
                );
            }
            if field.kind != "text" {
                continue;
            }
            for pattern in quoted_patterns() {
                for capture in pattern.captures_iter(&field.text) {
                    if let Some(spot) = capture.get(1) {
                        add(
                            episode,
                            field,
                            byte_to_char_offset(&field.text, spot.start()),
                            byte_to_char_offset(&field.text, spot.end()),
                            spot.as_str(),
                            "quoted_expression",
                            "explicit",
                        );
                    }
                }
            }
            // 平台 tag 在自由文本中的重现定位（展示高亮用）。
            if let Value::Array(tags) = get_value(&episode.platform_facts, "tags") {
                for tag_value in tags {
                    let tag = py_str(tag_value);
                    if tag.is_empty() {
                        continue;
                    }
                    let mut search_from = 0usize; // 字符偏移
                    while let Some(relative) =
                        char_slice(&field.text, search_from, char_len(&field.text)).find(&tag)
                    {
                        // char_slice 复制子串，find 返回子串内字节偏移 → 转字符
                        let index = search_from
                            + byte_to_char_offset(
                                &char_slice(&field.text, search_from, char_len(&field.text)),
                                relative,
                            );
                        add(
                            episode,
                            field,
                            index,
                            index + char_len(&tag),
                            &tag,
                            "platform_tag_in_text",
                            "platform",
                        );
                        search_from = index + char_len(&tag);
                    }
                }
            }
        }
    }
    seeds
}

/// `validate_span`：失败时返回与 Python 相同的错误文案。
pub fn validate_span(
    episode: &Episode,
    field_path: &str,
    text: &str,
    start: i64,
    end: i64,
) -> Option<String> {
    let source = match episode.field_text(field_path) {
        Some(source) => source,
        None => {
            return Some(format!(
                "episode {} has no field {}",
                episode.episode_id, field_path
            ));
        }
    };
    let total = char_len(source) as i64;
    if start < 0 || end > total || end <= start {
        return Some(format!(
            "invalid offsets for {}:{}",
            episode.episode_id, field_path
        ));
    }
    let actual = char_slice(source, start as usize, end as usize);
    if actual != text {
        return Some(format!(
            "span mismatch for {}:{}; expected exact substring {:?}, got {:?}",
            episode.episode_id, field_path, actual, text
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_sorts_keys_and_keeps_unicode() {
        let value: Value = serde_json::from_str(r#"{"b":1,"a":"异环"}"#).unwrap();
        assert_eq!(json_canon(&value), r#"{"a":"异环","b":1}"#);
        let nested: Value = serde_json::from_str(r#"{"z":[{"y":0.96}],"id":172}"#).unwrap();
        assert_eq!(json_canon(&nested), r#"{"id":172,"z":[{"y":0.96}]}"#);
    }

    #[test]
    fn py_str_zero_is_empty() {
        assert_eq!(py_str_int(0), "");
        assert_eq!(py_str_int(15), "15");
        assert_eq!(py_str(&Value::Null), "");
        assert_eq!(py_str(&Value::Bool(false)), "");
        assert_eq!(py_str(&Value::from(172)), "172");
    }

    #[test]
    fn repr_list_matches_python() {
        assert_eq!(py_repr_list(&[]), "");
        assert_eq!(py_repr_list(&["abc".into()]), "['abc']");
        assert_eq!(
            py_repr_list(&["mention:demo-1:6014e0304a0cd43a019bbebb".into()]),
            "['mention:demo-1:6014e0304a0cd43a019bbebb']"
        );
    }

    #[test]
    fn norm_and_safe_type() {
        assert_eq!(norm(" 异 环-游戏_ "), "异环游戏");
        assert_eq!(norm("City Pop/Rock"), "citypoprock");
        assert_eq!(safe_type("Game"), "game");
        assert_eq!(safe_type("音乐 综合"), "音乐_综合");
        assert_eq!(safe_type("---"), "concept");
    }

    #[test]
    fn now_iso_shape() {
        let stamp = now_iso();
        assert!(stamp.ends_with("+00:00"));
        assert_eq!(stamp.len(), 32); // YYYY-MM-DDTHH:MM:SS.ffffff+00:00
    }
}
