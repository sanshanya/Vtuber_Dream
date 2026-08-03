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
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

static NORM_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[\s\-_·•/\\]+"#).expect("static regex"));
static SAFE_TYPE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[^0-9a-zA-Z一-鿿]+"#).expect("static regex"));
static QUOTED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"《([^《》]{1,120})》").expect("static regex"),
        Regex::new(r"【([^【】]{1,120})】").expect("static regex"),
        Regex::new(r"#([^#\n]{1,80})#").expect("static regex"),
    ]
});

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
/// 注意：本函数是纯 `str(x)` 语义——数字 0 仍得 "0"（Python `str(0)`）。
/// 需要 `or` truthiness（0/0.0 落槽）的调用点请走 collector 的 `or_chain`/层本身，
/// 不要在本函数里改语义（hash_parts 的 `str(p or "")` 依赖此层的空串兜底）。
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
    NORM_REGEX.replace_all(&lowered, "").into_owned()
}

/// `_safe_type`：非 `0-9a-zA-Z一-鿿` 序列折叠为 `_`，去首尾 `_`，空 → "concept"。
pub fn safe_type(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let replaced = SAFE_TYPE_REGEX.replace_all(&lowered, "_");
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

mod build;
mod evidence;
mod seeds;

pub use build::{build_viewer_episodes, evidence_to_episode};
pub use evidence::viewer_evidence;
pub use seeds::{deterministic_mention_seeds, validate_span};

// ---------------------------------------------------------------------------
// Episode
// ---------------------------------------------------------------------------

pub(crate) fn get_value<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeField {
    pub path: String,
    pub text: String,
    pub kind: String,
}

impl EpisodeField {
    /// 图谱存储形态；字段名与 Python episode fields 记录一致（经 json_canon 排序后落库）。
    pub fn to_json(&self) -> Value {
        serde_json::json!({"path": self.path, "text": self.text, "kind": self.kind})
    }
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
