//! Collector 编排基底（移植 Python `collector.py` 的 normalize 集）。
//!
//! 移植策略（design M2）：normalize 集直连 Python 锚 fixture（tests-fixtures/m2/normalize.json）。
//! 编排层（collect_viewer 预算循环 / enrich 逐文件写回（仅动变更文件）/ collect() 主入口）拆到 `run/`。
//! 本节只到：**normalize 集 + _status/_content_id/_brief/collect_text**。
//!
//! 哲学红线（AGENTS.md「AI-first，但不是 AI-only」）：AI 毒探搜索委托给 `bilibili.rs`/`runtime.rs`，本模块只做**原样重组**
//! 公开字段，不进行任何语义推断。

use serde_json::Value;
use sha1::{Digest, Sha1};

// ---------------------------------------------------------------------------
// 语言惯性函数照抄
// ---------------------------------------------------------------------------

/// Python `_brief`：超限截断，附加 "…"。
pub fn brief(error: &dyn std::fmt::Display, limit: usize) -> String {
    // Python `_brief`：先 replace+strip，再按字符数截断加 "…"。
    let text = error.to_string().replace('\n', " ").trim().to_string();
    if text.chars().count() <= limit {
        text
    } else {
        let clipped: String = text.chars().take(limit - 1).collect();
        format!("{clipped}…")
    }
}

/// Python `_content_id`：sha1 hex[:16]。
pub fn content_id(source: &str, owner_id: &str, raw_id: &str, title: &str) -> String {
    let input = format!("{source}|{owner_id}|{raw_id}|{title}");
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..16].to_string()
}

/// Python `_status(status, items, detail)`。
pub fn status_row(status: &str, items: Vec<Value>, detail: &str) -> Value {
    serde_json::json!({
        "status": status,
        "count": items.len(),
        "detail": detail,
        "items": items,
    })
}

/// Python `_source_error`：hidden → "hidden"，其余 → "error"
/// detail 等于 Python 的 `str(error)`，包含 endpoint/code。
pub fn source_error_status(error: &crate::bilibili::BilibiliError) -> Value {
    let status = if error.hidden() {
        "hidden".to_string()
    } else {
        "error".to_string()
    };
    serde_json::json!({
        "status": status,
        "count": 0,
        "detail": error.to_string(),
        "items": [],
    })
}

// ---------------------------------------------------------------------------
// 公用取值辅助
// ---------------------------------------------------------------------------

fn strip_opt(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `str(item.get(k1) or item.get(k2) or "")`：串空槽位。
fn first_str(item: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = item.get(*key) {
            let s = strip_opt(Some(value));
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}

/// `str(item.get(k1) or item.get(k2) or "")`：Python `or` truthy——
/// `0`（未发布动态的 pub_ts=0！）、`None`、`False`、空串都必须落到下一个槽位。
fn first_number_str(item: &Value, keys: &[&str]) -> String {
    for key in keys {
        match item.get(*key) {
            Some(Value::Number(n)) => {
                if n.as_f64() != Some(0.0) {
                    return n.to_string();
                }
            }
            Some(Value::String(s)) => {
                let t = s.trim();
                if !t.is_empty() {
                    return t.to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// Python `str(x or "")`——会使 Python 的 None/0/"" → ""；0/整数会得到 Python str(0)="0"。
fn pystr(value: &Value) -> String {
    crate::episodes::py_str(value)
}

// ---------------------------------------------------------------------------
// normalize 集（与 Python collector.py:94-268 配对 fixture 钉）
// ---------------------------------------------------------------------------

/// profile endpoint 的采集结构（含 外层 stats）。
pub fn normalize_profile(uid: &str, profile: &Value, stats: &Value) -> Value {
    let official = profile
        .get("official")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    serde_json::json!({
        "uid": uid,
        "name": pystr(profile.get("name").unwrap_or(&Value::Null)),
        "face": pystr(profile.get("face").unwrap_or(&Value::Null)),
        "sign": pystr(profile.get("sign").unwrap_or(&Value::Null)),
        "level": profile.get("level").and_then(Value::as_i64).unwrap_or(0),
        "official_title": pystr(official.get("title").unwrap_or(&Value::Null)),
        "following": stats.get("following").and_then(Value::as_i64).unwrap_or(0),
        "followers": stats.get("follower").and_then(Value::as_i64).unwrap_or(0),
        "profile_url": format!("https://space.bilibili.com/{uid}"),
    })
}

pub fn normalize_followings(uid: &str, items: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for item in items {
        let creator_id = first_str(item, &["mid"]);
        if creator_id.is_empty() {
            continue;
        }
        let name = first_str(item, &["uname"]);
        let sign = first_str(item, &["sign"]);
        out.push(serde_json::json!({
            "id": content_id("following", uid, &creator_id, &name),
            "source": "following",
            "creator_id": creator_id,
            "creator_name": name,
            "title": name,
            "description": sign,
            "published_at": "",
            "url": format!("https://space.bilibili.com/{creator_id}"),
            "bvid": "",
            "tags": [],
        }));
    }
    out
}

pub fn normalize_videos(uid: &str, items: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for item in items {
        let bvid = first_str(item, &["bvid"]);
        let title = first_str(item, &["title"]);
        out.push(serde_json::json!({
            "id": content_id("video", uid, &bvid, &title),
            "source": "video",
            "creator_id": uid,
            "creator_name": "",
            "title": title,
            "description": pystr(item.get("description").unwrap_or(&Value::Null)),
            "published_at": first_number_str(item, &["created", "pubdate"]),
            "url": if bvid.is_empty() { String::new() } else { format!("https://www.bilibili.com/video/{bvid}") },
            "bvid": bvid,
            "tags": [],
        }));
    }
    out
}

pub fn normalize_dynamics(uid: &str, items: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for item in items {
        let dynamic_id = first_str(item, &["id_str", "id"]);
        let texts = collect_text(item);
        if texts.is_empty() {
            continue;
        }
        let bvid = find_first_value(item, "bvid")
            .into_iter()
            .find(|v| v.starts_with("BV"))
            .unwrap_or_default();
        let author = item
            .get("modules")
            .and_then(|m| m.get("module_author"))
            .cloned()
            .unwrap_or(Value::Null);
        out.push(serde_json::json!({
            "id": content_id("dynamic", uid, &dynamic_id, &texts[0]),
            "source": "dynamic",
            "creator_id": uid,
            "creator_name": "",
            "title": texts[0],
            "description": texts[1..].join(" "),
            "published_at": first_number_str(&author, &["pub_ts", "pub_time"]),
            "url": if dynamic_id.is_empty() { String::new() } else { format!("https://t.bilibili.com/{dynamic_id}") },
            "bvid": bvid,
            "tags": [],
        }));
    }
    out
}

pub fn normalize_favorites(uid: &str, folder: &Value, items: &[Value]) -> Vec<Value> {
    let folder_id = first_str(folder, &["id", "media_id"]);
    let folder_name = {
        let t = first_str(folder, &["title"]);
        if t.is_empty() {
            "收藏夹".to_string()
        } else {
            t
        }
    };
    let mut out = Vec::new();
    for item in items {
        let bvid = first_str(item, &["bvid", "bv_id"]);
        let title = first_str(item, &["title"]);
        let upper = item
            .get("upper")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let link = first_str(item, &["link"]);
        out.push(serde_json::json!({
            "id": content_id("favorite", uid, &format!("{folder_id}:{bvid}"), &title),
            "source": "favorite",
            "folder_id": folder_id,
            "folder_name": folder_name,
            "creator_id": pystr(upper.get("mid").unwrap_or(&Value::Null)),
            "creator_name": pystr(upper.get("name").unwrap_or(&Value::Null)),
            "title": title,
            "description": pystr(item.get("intro").unwrap_or(&Value::Null)),
            "published_at": first_number_str(item, &["fav_time", "pubtime"]),
            "url": if link.is_empty() {
                if bvid.is_empty() { String::new() } else { format!("https://www.bilibili.com/video/{bvid}") }
            } else {
                link
            },
            "bvid": bvid,
            "tags": [],
        }));
    }
    out
}

pub fn normalize_bangumi(uid: &str, items: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for item in items {
        let title = first_str(item, &["title", "season_title"]);
        if title.is_empty() {
            continue;
        }
        let style_names: Vec<String> = item
            .get("styles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|style| {
                let t = first_str(style, &["name"]);
                if t.is_empty() { None } else { Some(t) }
            })
            .collect();
        let season_id = pystr(item.get("season_id").unwrap_or(&Value::Null));
        let mut description = String::new();
        for frag in [
            first_str(item, &["subtitle"]),
            first_str(item, &["evaluate"]),
            style_names.join(" "),
        ] {
            if !frag.is_empty() {
                if !description.is_empty() {
                    description.push(' ');
                }
                description.push_str(&frag);
            }
        }
        let url = first_str(item, &["url"]);
        out.push(serde_json::json!({
            "id": content_id("bangumi", uid, &season_id, &title),
            "source": "bangumi",
            "creator_id": "",
            "creator_name": "",
            "title": title,
            "description": description,
            "published_at": "",
            "url": if url.is_empty() {
                if season_id.is_empty() { String::new() } else { format!("https://www.bilibili.com/bangumi/play/ss{season_id}") }
            } else {
                url
            },
            "bvid": "",
            "tags": style_names,
        }));
    }
    out
}

pub fn normalize_games(uid: &str, items: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let title = first_str(item, &["name", "title", "game_name"]);
        if title.is_empty() {
            continue;
        }
        let raw_id = {
            let s = first_number_str(item, &["game_id", "id"]);
            if s.is_empty() { index.to_string() } else { s }
        };
        let desc = {
            let s1 = first_str(item, &["summary"]);
            let s2 = first_str(item, &["description"]);
            let joined = [s1, s2]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                String::new()
            } else {
                joined
            }
        };
        out.push(serde_json::json!({
            "id": content_id("game", uid, &raw_id, &title),
            "source": "game",
            "creator_id": "",
            "creator_name": "",
            "title": title,
            "description": if desc.is_empty() { Value::String(String::new()) } else { Value::String(desc) },
            "published_at": "",
            "url": pystr(item.get("url").unwrap_or(&Value::Null)),
            "bvid": "",
            "tags": [],
        }));
    }
    out
}

// ---------------------------------------------------------------------------
// 文本核心扫掠（与 `collector.py`：_collect_text）
// ---------------------------------------------------------------------------

/// Python `_collect_text`：深度扫掠 Object/Array 中指定键的 str/数字值，保序去重。
/// （依赖 serde_json `preserve_order` 模拟 Python dict 插入序——fixture 钉的遍历序即是该语义）。
pub fn collect_text(value: &Value) -> Vec<String> {
    let allowed = ["text", "title", "desc", "description", "summary", "name"];
    let mut out: Vec<String> = Vec::new();
    collect_text_inner(value, allowed.as_slice(), &mut out);
    out
}

fn collect_text_inner(value: &Value, allowed: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if allowed.contains(&key.as_str())
                    && matches!(item, Value::String(_) | Value::Number(_))
                {
                    let text = strip_opt(Some(item));
                    if !text.is_empty() && !out.contains(&text) {
                        out.push(text);
                    }
                } else if matches!(item, Value::Object(_) | Value::Array(_)) {
                    collect_text_inner(item, allowed, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_text_inner(item, allowed, out);
            }
        }
        _ => {}
    }
}

/// `_find_values`：重点求 key 同名深度事实（Python 里仅取第一个 BV）。
fn find_first_value(value: &Value, wanted_key: &str) -> Vec<String> {
    let mut out = Vec::new();
    find_values_inner(value, wanted_key, &mut out);
    out
}

fn find_values_inner(value: &Value, wanted_key: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if key == wanted_key && matches!(item, Value::String(_) | Value::Number(_)) {
                    let text = strip_opt(Some(item));
                    if !text.is_empty() {
                        out.push(text);
                    }
                } else if matches!(item, Value::Object(_) | Value::Array(_)) {
                    find_values_inner(item, wanted_key, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                find_values_inner(item, wanted_key, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 编排层（run.rs：预算循环 / enrich / 主入口）
// ---------------------------------------------------------------------------

pub mod run;

pub use run::{CollectError, CollectMode, collect, collect_with_client};

// ---------------------------------------------------------------------------
// 仅两个测试钉钉：normalize parity fixture + 帮助测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod normalize_fixture_tests {
    use super::*;

    fn fixture() -> Value {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests-fixtures/m2/normalize.json");
        serde_json::from_str(&std::fs::read_to_string(root).unwrap()).unwrap()
    }

    #[test]
    fn parity_profile_followings_videos() {
        let fx = fixture();
        for case in fx["profile"].as_array().unwrap() {
            let args = &case["args"];
            assert_eq!(
                &normalize_profile(
                    args["uid"].as_str().unwrap(),
                    &args["profile"],
                    &args["stats"],
                ),
                &case["expected"]
            );
        }
        for (name, f) in [
            (
                "followings",
                normalize_followings as fn(&str, &[Value]) -> Vec<Value>,
            ),
            ("videos", normalize_videos as _),
        ] {
            for case in fx[name].as_array().unwrap() {
                let args = &case["args"];
                let items = args["items"].as_array().unwrap();
                assert_eq!(
                    &Value::Array(f(args["uid"].as_str().unwrap(), items)),
                    &case["expected"],
                    "{name} {case:?}"
                );
            }
        }
    }

    #[test]
    fn parity_dynamics_favorites_bangumi_games() {
        let fx = fixture();
        for case in fx["dynamics"].as_array().unwrap() {
            let args = &case["args"];
            let actual = normalize_dynamics(
                args["uid"].as_str().unwrap(),
                args["items"].as_array().unwrap(),
            );
            assert_eq!(&Value::Array(actual), &case["expected"], "{case:?}");
        }
        for case in fx["favorites"].as_array().unwrap() {
            let args = &case["args"];
            let actual = normalize_favorites(
                args["uid"].as_str().unwrap(),
                &args["folder"],
                args["items"].as_array().unwrap(),
            );
            assert_eq!(&Value::Array(actual), &case["expected"], "{case:?}");
        }
        for case in fx["bangumi"].as_array().unwrap() {
            let args = &case["args"];
            let actual = normalize_bangumi(
                args["uid"].as_str().unwrap(),
                args["items"].as_array().unwrap(),
            );
            assert_eq!(&Value::Array(actual), &case["expected"], "{case:?}");
        }
        for case in fx["games"].as_array().unwrap() {
            let args = &case["args"];
            let actual = normalize_games(
                args["uid"].as_str().unwrap(),
                args["items"].as_array().unwrap(),
            );
            assert_eq!(&Value::Array(actual), &case["expected"], "{case:?}");
        }
    }

    #[test]
    fn parity_collect_text() {
        let fx = fixture();
        for case in fx["collect_text"].as_array().unwrap() {
            let actual: Vec<Value> = collect_text(&case["input"])
                .into_iter()
                .map(Value::String)
                .collect();
            assert_eq!(&Value::Array(actual), &case["expected"], "{case:?}");
        }
    }

    #[test]
    fn hidden_vs_error_in_source_error() {
        // 手动构造 hidden + error：status → hidden/error
        let hidden = crate::bilibili::BilibiliError::Api {
            endpoint: String::new(),
            code: 22115,
            message: String::new(),
        };
        let normal = crate::bilibili::BilibiliError::Api {
            endpoint: String::new(),
            code: -352,
            message: String::new(),
        };
        assert_eq!(source_error_status(&hidden)["status"], "hidden");
        assert!(source_error_status(&normal)["status"] != "hidden");
    }
}
