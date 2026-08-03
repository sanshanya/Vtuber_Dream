//! collect() 编排（移植 Python `collector.py` 的 `_collect_viewer / _enrich_video_metadata /
//! _write_platform_snapshot / _collect_streamer / collect`）。
//!
//! 预算纪律（与 Python 逐行对齐）：
//! - profile / relation_stat 手工特判（不走 `_call_source`）。
//! - followings/videos/dynamics/bangumi/games 走 `call_source`（1 请求/源）。
//! - favorites 嵌套预算：folders 列表 1 请求 + 每个公开收藏夹 items 各 1 请求，逐次记账。
//! - 错误隔离：单源失败只落 `source.status`（hidden/error），不中断观众与整体采集
//!   （失败只影响当前工作单元，AGENTS.md §3）。

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::bilibili::{BilibiliClient, BilibiliError};
use crate::config::Config;
use crate::episodes::now_iso;
use crate::storage;

use super::{
    brief, normalize_bangumi, normalize_dynamics, normalize_favorites, normalize_followings,
    normalize_games, normalize_profile, normalize_videos, source_error_status, status_row,
};

#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    #[error("{0}")]
    Client(#[from] BilibiliError),
    #[error("{0}")]
    Storage(String),
    #[error("{0}")]
    Message(String),
}

fn pystr(value: Option<&Value>) -> String {
    crate::episodes::py_str(value.unwrap_or(&Value::Null))
}

/// Python `int(x or 0)`：数字直取，字符串可解析，其余 0。
fn py_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Python `or` 语义取第一个 truthy 字符串（0/None/False/"" 全 falsy）。
fn or_chain(values: &[&Value]) -> String {
    for value in values {
        let text = pystr(Some(value));
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

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
                let mut row = status_row(status, favorites_rows.clone(), &detail);
                row["folders"] = Value::Array(favorite_folders.clone());
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

// ---------------------------------------------------------------------------
// enrich：对观察到的视频回灌 B站 TAG/分区（Python `_enrich_video_metadata`）
// ---------------------------------------------------------------------------

/// Python `_all_content_items`：六源 items 依次展开（只收对象）。
pub fn all_content_items(viewer: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    if let Some(sources) = viewer.get("sources").and_then(Value::as_object) {
        for source in [
            "followings",
            "videos",
            "dynamics",
            "favorites",
            "bangumi",
            "games",
        ] {
            if let Some(items) = sources
                .get(source)
                .and_then(|v| v.get("items"))
                .and_then(Value::as_array)
            {
                result.extend(items.iter().filter(|i| i.is_object()).cloned());
            }
        }
    }
    result
}

const ENRICH_SOURCE_PRIORITY: [(&str, i64); 3] = [("favorite", 0), ("dynamic", 1), ("video", 2)];

pub fn enrich_video_metadata(
    client: &mut BilibiliClient,
    root: &Path,
    limit: i64,
    emit: &mut dyn FnMut(&str),
) -> Result<Value, CollectError> {
    let viewers = storage::load_viewers(root).map_err(CollectError::Storage)?;
    let cache_path = root.join("shared").join("video_metadata.json");
    let mut cache = Map::new();

    let mut candidates: Vec<(i64, String)> = Vec::new();
    for viewer in &viewers {
        for item in all_content_items(viewer) {
            let bvid = pystr(item.get("bvid")).trim().to_string();
            let source = pystr(item.get("source"));
            if !bvid.is_empty() {
                let priority = ENRICH_SOURCE_PRIORITY
                    .iter()
                    .find(|(name, _)| *name == source)
                    .map(|(_, p)| *p)
                    .unwrap_or(9);
                candidates.push((priority, bvid));
            }
        }
    }
    candidates.sort();
    let mut seen = BTreeSet::new();
    let unique: Vec<String> = candidates
        .into_iter()
        .filter_map(|(_, bvid)| seen.insert(bvid.clone()).then_some(bvid))
        .collect();

    let selected: Vec<&String> = unique.iter().take(limit.max(0) as usize).collect();
    if !selected.is_empty() {
        emit(&format!(
            "[4/5] 同步B站视频TAG与分区：{} 个视频",
            selected.len()
        ));
    }
    for (index, bvid) in selected.iter().enumerate() {
        let mut detail = Value::Null;
        let mut tags: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        match client.video_detail(bvid) {
            Ok(value) => detail = value,
            Err(err) => errors.push(brief(&err, 100)),
        }
        match client.video_tags(bvid) {
            Ok(value) => tags = value,
            Err(err) => errors.push(brief(&err, 100)),
        }
        let category = json!({
            "id": py_int(detail.get("tid")),
            "name": pystr(detail.get("tname")),
            "parent_id": py_int(detail.get("parent_tid")),
            "v2_name": pystr(detail.get("tname_v2")),
        });
        let owner = detail
            .get("owner")
            .filter(|o| o.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let category_name = pystr(category.get("name"));
        cache.insert(
            bvid.to_string(),
            json!({
                "bvid": bvid,
                "title": pystr(detail.get("title")),
                "pubdate": detail.get("pubdate").cloned().unwrap_or(Value::Null),
                "owner": owner,
                "category": category,
                "tags": tags.clone(),
                "captured_at": now_iso(),
                "errors": errors,
            }),
        );
        emit(&format!(
            "[4/5] 视频元数据 {}/{} {}：分区={}，TAG={}",
            index + 1,
            selected.len(),
            bvid,
            if category_name.is_empty() {
                "未知"
            } else {
                &category_name
            },
            tags.len()
        ));
        storage::write_json(&cache_path, &Value::Object(cache.clone()))
            .map_err(CollectError::Storage)?;
    }

    // 回写 viewers（Python：逐文件、变了才写）
    let mut viewers = viewers;
    for viewer in &mut viewers {
        let mut changed = false;
        if let Some(sources) = viewer.get_mut("sources").and_then(Value::as_object_mut) {
            for source in [
                "followings",
                "videos",
                "dynamics",
                "favorites",
                "bangumi",
                "games",
            ] {
                if let Some(items) = sources
                    .get_mut(source)
                    .and_then(|v| v.get_mut("items"))
                    .and_then(Value::as_array_mut)
                {
                    for item in items.iter_mut().filter(|i| i.is_object()) {
                        let bvid = pystr(item.get("bvid")).trim().to_string();
                        let Some(metadata) = cache.get(&bvid) else {
                            continue;
                        };
                        let tags_value = metadata.get("tags").cloned().unwrap_or(json!([]));
                        let category_value = metadata.get("category").cloned().unwrap_or(json!({}));
                        if item.get("tags") != Some(&tags_value) {
                            item["tags"] = tags_value;
                            changed = true;
                        }
                        if item.get("platform_category") != Some(&category_value) {
                            item["platform_category"] = category_value;
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            let uid = pystr(viewer.pointer("/viewer/id"));
            storage::write_json(&root.join("viewers").join(format!("{uid}.json")), viewer)
                .map_err(CollectError::Storage)?;
        }
    }
    Ok(Value::Object(cache))
}

// ---------------------------------------------------------------------------
// platform snapshot / streamer
// ---------------------------------------------------------------------------

/// Python `_write_platform_snapshot`：只收录本轮观察到的分区/TAG/热搜（事实，非人工词表）。
pub fn write_platform_snapshot(
    root: &Path,
    metadata: &Value,
    hot_searches: Vec<Value>,
) -> Result<Value, CollectError> {
    let mut categories = Map::new();
    let mut tags = BTreeSet::new();
    if let Some(items) = metadata.as_object() {
        for item in items.values() {
            let category = item.get("category").filter(|c| c.is_object()).cloned();
            if let Some(category) = category {
                let identity = or_chain(&[&category["id"], &category["name"]])
                    .trim()
                    .to_string();
                if !identity.is_empty() {
                    categories.insert(identity, category);
                }
            }
            for tag in item
                .get("tags")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let text = pystr(Some(&tag)).trim().to_string();
                if !text.is_empty() {
                    tags.insert(text);
                }
            }
        }
    }
    let snapshot = json!({
        "platform": "bilibili",
        "captured_at": now_iso(),
        "observed_video_categories": categories.values().cloned().collect::<Vec<_>>(),
        "observed_video_tags": tags.into_iter().collect::<Vec<_>>(),
        "hot_searches": hot_searches,
        "video_metadata_count": metadata.as_object().map_or(0, Map::len),
        "note": "仅保存本次公开观测涉及的B站分区、TAG与热搜，不使用人工固定兴趣词表。",
    });
    storage::write_json(
        &root.join("shared").join("platform_snapshot.json"),
        &snapshot,
    )
    .map_err(CollectError::Storage)?;
    Ok(snapshot)
}

/// Python `_collect_streamer`：主播近期内容，各源独立容错；TAG/分区从 metadata_cache 回灌。
pub fn collect_streamer(
    client: &mut BilibiliClient,
    config: &Config,
    metadata_cache: &Value,
) -> Value {
    let uid = &config.bilibili.streamer_uid;
    let mut statuses = Map::new();
    let profile = match client.profile(uid) {
        Ok(value) => {
            statuses.insert("profile".into(), Value::String("ok".into()));
            value
        }
        Err(err) => {
            statuses.insert("profile".into(), Value::String(err.to_string()));
            Value::Null
        }
    };
    let stats = match client.relation_stat(uid) {
        Ok(value) => {
            statuses.insert("relation_stat".into(), Value::String("ok".into()));
            value
        }
        Err(err) => {
            statuses.insert("relation_stat".into(), Value::String(err.to_string()));
            Value::Null
        }
    };
    let mut videos = match client.videos(uid, config.collection.recent_videos.max(10)) {
        Ok(items) => {
            statuses.insert("videos".into(), Value::String("ok".into()));
            normalize_videos(uid, &items)
        }
        Err(err) => {
            statuses.insert("videos".into(), Value::String(err.to_string()));
            Vec::new()
        }
    };
    let mut dynamics = match client.dynamics(uid, config.collection.recent_dynamics.max(10)) {
        Ok(items) => {
            statuses.insert("dynamics".into(), Value::String("ok".into()));
            normalize_dynamics(uid, &items)
        }
        Err(err) => {
            statuses.insert("dynamics".into(), Value::String(err.to_string()));
            Vec::new()
        }
    };
    for item in videos.iter_mut().chain(dynamics.iter_mut()) {
        let bvid = pystr(item.get("bvid"));
        if let Some(metadata) = metadata_cache.get(&bvid) {
            item["tags"] = metadata.get("tags").cloned().unwrap_or(json!([]));
            item["platform_category"] = metadata.get("category").cloned().unwrap_or(json!({}));
        }
    }
    json!({
        "profile": normalize_profile(uid, &profile, &stats),
        "sources": {"videos": videos, "dynamics": dynamics},
        "statuses": Value::Object(statuses),
    })
}

// ---------------------------------------------------------------------------
// collect() 主入口（Python `collect`）
// ---------------------------------------------------------------------------

pub fn collect(config: &Config, emit: &mut dyn FnMut(&str)) -> Result<Value, CollectError> {
    let client = BilibiliClient::new(
        &config.bilibili.cookie,
        config.collection.request_delay_seconds,
        config.collection.timeout_seconds,
    )?;
    collect_with_client(client, config, emit)
}

pub fn collect_with_client(
    mut client: BilibiliClient,
    config: &Config,
    emit: &mut dyn FnMut(&str),
) -> Result<Value, CollectError> {
    let root = config.output_dir.clone();
    if config.perception.preserve_raw_snapshots
        && let Some(archived) =
            storage::archive_current_snapshot(&root).map_err(CollectError::Storage)?
    {
        emit(&format!("[0/5] 已归档上一轮快照：{}", archived.display()));
    }
    storage::reset_output(&root).map_err(CollectError::Storage)?;
    let started_at = now_iso();
    let started = Instant::now();
    storage::write_json(
        &root.join("collection.json"),
        &json!({"status": "running", "started_at": started_at}),
    )
    .map_err(CollectError::Storage)?;

    match collect_inner(&mut client, config, emit, &started_at, started) {
        Ok(summary) => {
            storage::write_json(&root.join("collection.json"), &summary)
                .map_err(CollectError::Storage)?;
            emit(&format!(
                "完成：{} 名观众，{} 条表面信息，{} 次请求，{:.2} 秒",
                summary["viewer_count"].as_i64().unwrap_or(0),
                summary["content_item_count"].as_i64().unwrap_or(0),
                summary["request_count"].as_i64().unwrap_or(0),
                summary["elapsed_seconds"].as_f64().unwrap_or(0.0),
            ));
            Ok(summary)
        }
        Err(err) => {
            let _ = storage::write_json(
                &root.join("collection.json"),
                &json!({
                    "status": "failed",
                    "started_at": started_at,
                    "updated_at": now_iso(),
                    "detail": err.to_string(),
                }),
            );
            Err(err)
        }
    }
}

fn bump_counter(counter: &mut BTreeMap<String, i64>, key: String) {
    *counter.entry(key).or_insert(0) += 1;
}

fn collect_inner(
    client: &mut BilibiliClient,
    config: &Config,
    emit: &mut dyn FnMut(&str),
    started_at: &str,
    started: Instant,
) -> Result<Value, CollectError> {
    emit("[1/5] 验证 B站登录状态");
    let auth = client.auth_status()?;
    if !auth
        .get("is_login")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CollectError::Message("Cookie is not logged in".into()));
    }

    emit("[2/5] 获取大航海与手工指定观众");
    let guards = client.guard_members(
        &config.bilibili.room_id,
        &config.bilibili.streamer_uid,
        config.collection.max_guards,
    )?;
    let mut seeds: Vec<(String, Value)> = Vec::new();
    for item in &guards {
        let uid = pystr(item.get("uid"));
        if uid.is_empty() {
            continue;
        }
        seeds.push((
            uid.clone(),
            json!({
                "id": uid,
                "name": item.get("name").cloned().unwrap_or(json!("")),
                "face": item.get("face").cloned().unwrap_or(json!("")),
                "guard_level": item.get("guard_level").cloned().unwrap_or(json!(0)),
                "medal_level": item.get("medal_level").cloned().unwrap_or(json!(0)),
                "seed_source": "guard",
            }),
        ));
    }
    for uid in &config.bilibili.additional_viewer_ids {
        if !seeds.iter().any(|(id, _)| id == uid) {
            seeds.push((
                uid.clone(),
                json!({
                    "id": uid,
                    "name": "",
                    "face": "",
                    "guard_level": 0,
                    "medal_level": 0,
                    "seed_source": "manual",
                }),
            ));
        }
    }
    if seeds.is_empty() {
        return Err(CollectError::Message("no usable viewers were found".into()));
    }
    emit(&format!("[2/5] 观众种子：{} 人", seeds.len()));

    let mut source_counter: BTreeMap<String, i64> = BTreeMap::new();
    let mut counter_order: Vec<String> = Vec::new();
    let mut evidence_counter: i64 = 0;
    for (index, (_, base)) in seeds.iter().enumerate() {
        let viewer = collect_viewer(client, base, config);
        let uid = pystr(viewer.pointer("/viewer/id"));
        storage::write_json(
            &config
                .output_dir
                .join("viewers")
                .join(format!("{uid}.json")),
            &viewer,
        )
        .map_err(CollectError::Storage)?;
        let mut parts: Vec<String> = Vec::new();
        if let Some(sources) = viewer.get("sources").and_then(Value::as_object) {
            for (name, source) in sources {
                let status = pystr(source.get("status"));
                // 首次出现才追加顺序表（保 Python Counter dict 的插入序）
                let key = format!("{name}:{status}");
                if !source_counter.contains_key(&key) {
                    counter_order.push(key.clone());
                }
                bump_counter(&mut source_counter, key);
                let count = source.get("count").and_then(Value::as_i64).unwrap_or(0);
                evidence_counter += count;
                if name != "coins" && name != "likes" && name != "relation_stat" {
                    parts.push(format!("{name}={status}/{count}"));
                }
            }
        }
        let name = pystr(viewer.pointer("/viewer/name"));
        emit(&format!(
            "[3/5] 观众 {}/{} {}：{}",
            index + 1,
            seeds.len(),
            name,
            parts.join("，")
        ));
    }

    let metadata_cache = enrich_video_metadata(
        client,
        &config.output_dir,
        config.collection.max_video_metadata_items,
        emit,
    )?;
    let hot_searches_count;
    let hot_searches = match client.hot_searches(config.perception.platform_hot_search_limit) {
        Ok(rows) => {
            hot_searches_count = rows.len();
            rows
        }
        Err(err) => {
            hot_searches_count = 0;
            emit(&format!("[4/5] B站热搜同步失败：{}", brief(&err, 100)));
            Vec::new()
        }
    };
    write_platform_snapshot(&config.output_dir, &metadata_cache, hot_searches)?;

    emit("[5/5] 采集主播近期内容");
    let streamer = collect_streamer(client, config, &metadata_cache);
    storage::write_json(&config.output_dir.join("streamer.json"), &streamer)
        .map_err(CollectError::Storage)?;

    let elapsed = ((started.elapsed().as_secs_f64()) * 100.0).round() / 100.0;
    let guard_uid_set: BTreeSet<String> = guards.iter().map(|g| pystr(g.get("uid"))).collect();
    let mut status_counts = Map::new();
    for key in &counter_order {
        status_counts.insert(key.clone(), Value::from(source_counter[key]));
    }
    Ok(json!({
        "status": "complete",
        "project": config.project_name,
        "authenticated_uid": pystr(auth.get("mid")),
        "viewer_count": seeds.len() as i64,
        "guard_count": guards.len() as i64,
        "manual_viewer_count": seeds.len() as i64 - guard_uid_set.len() as i64,
        "content_item_count": evidence_counter,
        "video_metadata_items": metadata_cache.as_object().map_or(0, Map::len) as i64,
        "platform_hot_searches": hot_searches_count as i64,
        "request_count": client.request_count() as i64,
        "elapsed_seconds": elapsed,
        "source_status_counts": Value::Object(status_counts),
        "started_at": started_at,
        "finished_at": now_iso(),
    }))
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
