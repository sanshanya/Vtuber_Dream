//! 观众级采集：call_source 预算记账 + collect_viewer 组装（Python `_collect_viewer`）。

use serde_json::{Map, Value, json};

use crate::bilibili::{BilibiliClient, BilibiliError};
use crate::config::Config;
use crate::episodes::now_iso;

use super::super::{
    normalize_bangumi, normalize_dynamics, normalize_favorites, normalize_followings,
    normalize_games, normalize_profile, normalize_videos, source_error_status, status_row,
};
use super::{or_chain, py_int, pystr};

// ---------------------------------------------------------------------------
// collect_viewer（Python `_collect_viewer`）
// ---------------------------------------------------------------------------

/// Python `_call_source`：预算耗尽不调回调（budget_skipped）；结果记账 +1。
fn call_source(
    budget: i64,
    used: i64,
    fetch: impl FnOnce() -> Result<Vec<Value>, BilibiliError>,
) -> (Value, i64) {
    if used >= budget {
        return (
            status_row(
                "budget_skipped",
                Vec::new(),
                "per-viewer request budget exhausted",
            ),
            used,
        );
    }
    match fetch() {
        Ok(rows) => (
            status_row(if rows.is_empty() { "empty" } else { "ok" }, rows, ""),
            used + 1,
        ),
        Err(err) => (source_error_status(&err), used + 1),
    }
}

/// 单源报错行（profile/relation_stat 用手工 dict：无 items 键——与 Python 字面量一致）。
/// 轮2-R1-B2 互指：collector/mod.rs 的 source_error_status 是本件的「含 items」变体——
/// 键差是 Python 字节 parity 承重（两边各自对齐 Python 字面量），禁止合并。
fn simple_error_row(err: &BilibiliError) -> Value {
    json!({
        "status": if err.hidden() { "hidden" } else { "error" },
        "count": 0,
        "detail": err.to_string(),
    })
}

pub fn collect_viewer(client: &mut BilibiliClient, base: &Value, config: &Config) -> Value {
    let uid = pystr(base.get("id"));
    let settings = &config.collection;
    let budget = settings.per_viewer_request_budget;
    let mut used: i64 = 0;
    let mut sources = Map::new();

    let mut profile_data = Value::Null;
    if used < budget {
        match client.profile(&uid) {
            Ok(value) => {
                profile_data = value;
                sources.insert(
                    "profile".into(),
                    json!({"status": "ok", "count": 1, "detail": ""}),
                );
            }
            Err(err) => {
                sources.insert("profile".into(), simple_error_row(&err));
            }
        }
        used += 1;
    } else {
        sources.insert(
            "profile".into(),
            json!({"status": "budget_skipped", "count": 0, "detail": "budget exhausted"}),
        );
    }

    let mut stats_data = Value::Null;
    if used < budget {
        match client.relation_stat(&uid) {
            Ok(value) => {
                stats_data = value;
                sources.insert(
                    "relation_stat".into(),
                    json!({"status": "ok", "count": 1, "detail": ""}),
                );
            }
            Err(err) => {
                sources.insert("relation_stat".into(), simple_error_row(&err));
            }
        }
        used += 1;
    } else {
        sources.insert(
            "relation_stat".into(),
            json!({"status": "budget_skipped", "count": 0, "detail": "budget exhausted"}),
        );
    }

    let row;
    (row, used) = call_source(budget, used, || {
        client
            .followings(&uid, settings.followings_limit)
            .map(|items| normalize_followings(&uid, &items))
    });
    sources.insert("followings".into(), row);

    let row;
    (row, used) = call_source(budget, used, || {
        client
            .videos(&uid, settings.recent_videos)
            .map(|items| normalize_videos(&uid, &items))
    });
    sources.insert("videos".into(), row);

    let row;
    (row, used) = call_source(budget, used, || {
        client
            .dynamics(&uid, settings.recent_dynamics)
            .map(|items| normalize_dynamics(&uid, &items))
    });
    sources.insert("dynamics".into(), row);

    // favorites：folders 列表 + 逐收藏夹 items，嵌套预算
    let mut favorites_rows: Vec<Value> = Vec::new();
    let mut favorite_folders: Vec<Value> = Vec::new();
    let favorites_row;
    if used < budget {
        match client.favorite_folders(&uid, settings.favorite_folders) {
            Ok(raw_folders) => {
                used += 1;
                let mut folder_errors: Vec<String> = Vec::new();
                for folder in &raw_folders {
                    if used >= budget {
                        folder_errors.push(
                            "request budget exhausted before all public folders were read".into(),
                        );
                        break;
                    }
                    let media_id = or_chain(&[&folder["id"], &folder["media_id"]])
                        .trim()
                        .to_string();
                    if media_id.is_empty() {
                        continue;
                    }
                    match client.favorite_items(&media_id, settings.favorite_items_per_folder) {
                        Ok(raw_items) => {
                            used += 1;
                            let title = {
                                let t = pystr(folder.get("title"));
                                if t.is_empty() {
                                    "收藏夹".to_string()
                                } else {
                                    t
                                }
                            };
                            let media_count = folder.get("media_count");
                            favorite_folders.push(json!({
                                "id": media_id,
                                "title": title,
                                "count": if py_int(media_count) != 0 { py_int(media_count) } else { raw_items.len() as i64 },
                            }));
                            favorites_rows.extend(normalize_favorites(&uid, folder, &raw_items));
                        }
                        Err(err) => {
                            used += 1;
                            folder_errors.push(err.to_string());
                        }
                    }
                }
                let status = if !favorites_rows.is_empty() && !folder_errors.is_empty() {
                    "partial"
                } else if !favorites_rows.is_empty() {
                    "ok"
                } else if !folder_errors.is_empty() {
                    "error"
                } else {
                    "empty"
                };
                let detail: String = folder_errors.join("; ").chars().take(500).collect();
                let mut row = status_row(status, favorites_rows, &detail);
                row["folders"] = Value::Array(favorite_folders);
                favorites_row = row;
            }
            Err(err) => {
                used += 1;
                let mut row = source_error_status(&err);
                row["folders"] = Value::Array(Vec::new());
                favorites_row = row;
            }
        }
    } else {
        let mut row = status_row(
            "budget_skipped",
            Vec::new(),
            "per-viewer request budget exhausted",
        );
        row["folders"] = Value::Array(Vec::new());
        favorites_row = row;
    }
    sources.insert("favorites".into(), favorites_row);

    let row;
    (row, used) = call_source(budget, used, || {
        client
            .bangumi(&uid, settings.bangumi_limit)
            .map(|items| normalize_bangumi(&uid, &items))
    });
    sources.insert("bangumi".into(), row);

    let row;
    (row, used) = call_source(budget, used, || {
        client
            .games(&uid, settings.games_limit)
            .map(|items| normalize_games(&uid, &items))
    });
    sources.insert("games".into(), row);

    sources.insert(
        "coins".into(),
        json!({
            "status": "unsupported",
            "count": 0,
            "detail": "other users' coin history is not exposed as a public list",
            "items": [],
        }),
    );
    sources.insert(
        "likes".into(),
        json!({
            "status": "unsupported",
            "count": 0,
            "detail": "other users' like history is not exposed as a public list",
            "items": [],
        }),
    );

    let name = or_chain(&[&profile_data["name"], &base["name"]]);
    let name = if name.is_empty() { uid.clone() } else { name };
    let face = or_chain(&[&profile_data["face"], &base["face"]]);
    let seed_source = {
        let s = pystr(base.get("seed_source"));
        if s.is_empty() { "guard".to_string() } else { s }
    };
    let mut public_profile = normalize_profile(&uid, &profile_data, &stats_data);
    public_profile["name"] = Value::String(name.clone());
    public_profile["face"] = Value::String(face.clone());

    json!({
        "schema_version": 1,
        "collected_at": now_iso(),
        "viewer": {
            "id": uid,
            "name": name,
            "face": face,
            "profile_url": format!("https://space.bilibili.com/{uid}"),
            "guard_level": py_int(base.get("guard_level")),
            "medal_level": py_int(base.get("medal_level")),
            "seed_source": seed_source,
        },
        "profile": public_profile,
        "sources": Value::Object(sources),
        "request_budget": budget,
        "source_operations_used": used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_source_budget_skipped_never_calls_fetch() {
        let called = std::cell::Cell::new(false);
        let (row, used) = call_source(2, 2, || {
            called.set(true);
            Ok(vec![json!({"x": 1})])
        });
        assert!(!called.get());
        assert_eq!(used, 2);
        assert_eq!(row["status"], "budget_skipped");
        assert_eq!(row["items"], json!([]));
    }

    #[test]
    fn call_source_maps_hidden_and_consumes_budget() {
        let (row, used) = call_source(2, 0, || {
            Err(BilibiliError::Api {
                endpoint: "/x/e".into(),
                code: 22115,
                message: "隐私".into(),
            })
        });
        assert_eq!(used, 1);
        assert_eq!(row["status"], "hidden");
        assert_eq!(row["count"], 0);
        assert!(row["detail"].as_str().unwrap().contains("22115"));
    }

    #[test]
    fn call_source_ok_empty_marking() {
        let (row, used) = call_source(2, 0, || Ok(Vec::new()));
        assert_eq!((row["status"].as_str().unwrap(), used), ("empty", 1));
        let (row, used) = call_source(2, 0, || Ok(vec![json!({"a": 1})]));
        assert_eq!(
            (
                row["status"].as_str().unwrap(),
                row["count"].as_i64().unwrap(),
                used
            ),
            ("ok", 1, 1)
        );
    }
}
