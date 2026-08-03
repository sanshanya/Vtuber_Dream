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
    brief, content_id, normalize_bangumi, normalize_dynamics, normalize_favorites,
    normalize_followings, normalize_games, normalize_profile, normalize_videos,
    source_error_status, status_row,
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
        // Python `or` truthiness：数字 0 同样是 falsy（防幽灵分区 id=0 / 收藏夹 id=0）。
        if matches!(value, Value::Number(n) if n.as_f64() == Some(0.0)) {
            continue;
        }
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
// collect() 主入口（Python `collect` + design M2 模式化入口）
// ---------------------------------------------------------------------------

/// 采集模式（design M2：streamer-only 是 kind=collect 的默认；深采池扩张只走显式入口）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectMode {
    /// 只深采主播一人 + 房间级语料（评论区浅存在 / 回放弹幕）——冷启动默认。
    StreamerOnly,
    /// 大航海名单 + 手工观众全量（一键分析全部大航海入口；当前 Python 行为）。
    Guards,
    /// 单查按钮：只深采指定 uid（seed_source=manual）。
    SingleViewer(String),
}

pub fn collect(config: &Config, emit: &mut dyn FnMut(&str)) -> Result<Value, CollectError> {
    let client = BilibiliClient::new(
        &config.bilibili.cookie,
        config.collection.request_delay_seconds,
        config.collection.timeout_seconds,
    )?;
    collect_with_client(client, config, CollectMode::StreamerOnly, emit)
}

pub fn collect_with_client(
    mut client: BilibiliClient,
    config: &Config,
    mode: CollectMode,
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

    match collect_inner(&mut client, config, &mode, emit, &started_at, started) {
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

// ---------------------------------------------------------------------------
// 房间级浅存在采集点（design M2-B2c；每个点独立记账，失败隔离，不进深采池）
// ---------------------------------------------------------------------------

const DISCOVERY_LIMIT: i64 = 3;
const ROOM_COMMENT_PAGE_SIZE: i64 = 20;
const RECORD_LIST_PAGE_SIZE: i64 = 20;

/// 评论区浅存在（type=1 视频 oid=avid、type=17 动态 oid=动态数字 id；第一页，无 wbi）。
/// 返回 (payload_value, 请求数, 评论行数)。
fn collect_room_comments(
    client: &mut BilibiliClient,
    config: &Config,
) -> Result<(Value, i64, i64), CollectError> {
    let uid = &config.bilibili.streamer_uid;
    let budget = config.collection.room_comment_request_budget;
    let started_requests = client.request_count() as i64;
    let mut targets: Vec<(i64, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    // 目标发现：主播近期视频 aid + 动态 id（各取 DISCOVERY_LIMIT 条）
    match client.videos(uid, DISCOVERY_LIMIT) {
        Ok(items) => {
            for item in &items {
                let aid = pystr(item.get("aid"));
                if !aid.is_empty() {
                    targets.push((1, aid));
                }
            }
        }
        Err(err) => errors.push(brief(&err, 100)),
    }
    match client.dynamics(uid, DISCOVERY_LIMIT) {
        Ok(items) => {
            for item in &items {
                let mut id = pystr(item.get("id_str"));
                if id.is_empty() {
                    id = pystr(item.get("id"));
                }
                if !id.is_empty() {
                    targets.push((17, id));
                }
            }
        }
        Err(err) => errors.push(brief(&err, 100)),
    }
    let mut rows: Vec<Value> = Vec::new();
    let mut fetched = 0;
    for (type_id, oid) in targets.iter().take(budget.max(0) as usize) {
        match client.replies(oid, *type_id, ROOM_COMMENT_PAGE_SIZE) {
            Ok(items) => {
                fetched += 1;
                for item in &items {
                    rows.push(normalize_comment(
                        &config.bilibili.room_id,
                        *type_id,
                        oid,
                        item,
                    ));
                }
            }
            Err(err) => errors.push(brief(&err, 100)),
        }
    }
    let status = if fetched > 0 && errors.is_empty() {
        if rows.is_empty() { "empty" } else { "ok" }
    } else if fetched > 0 {
        "partial"
    } else if !errors.is_empty() {
        "error"
    } else {
        "empty"
    };
    let payload = json!({
        "platform": "bilibili",
        "captured_at": now_iso(),
        "streamer_uid": uid,
        "status": status,
        "budget": budget,
        "targets": targets
            .iter()
            .take(budget.max(0) as usize)
            .map(|(type_id, oid)| json!({"type": type_id, "oid": oid}))
            .collect::<Vec<_>>(),
        "request_count": client.request_count() as i64 - started_requests,
        "count": rows.len() as i64,
        "errors": errors,
        "rows": rows,
    });
    storage::write_json(
        &config.output_dir.join("shared").join("room_comments.json"),
        &payload,
    )
    .map_err(CollectError::Storage)?;
    let used = payload["request_count"].as_i64().unwrap_or(0);
    let count = payload["count"].as_i64().unwrap_or(0);
    Ok((payload, used, count))
}

fn normalize_comment(room_id: &str, type_id: i64, oid: &str, item: &Value) -> Value {
    let kind = if type_id == 1 { "video" } else { "dynamic" };
    let owner = format!("room:{room_id}");
    let mut rpid = pystr(item.get("rpid_str"));
    if rpid.is_empty() {
        rpid = pystr(item.get("rpid"));
    }
    let member = item.get("member").cloned().unwrap_or(Value::Null);
    let message = pystr(item.pointer("/content/message"));
    json!({
        "id": content_id("comment", &owner, &rpid, &message),
        "source": "comment",
        "target_kind": kind,
        "target_oid": oid,
        "rpid": rpid,
        "mid": pystr(member.get("mid")),
        "uname": pystr(member.get("uname")),
        "message": message,
        "like": py_int(item.get("like")),
        "rcount": py_int(item.get("rcount")),
        "ctime": pystr(item.get("ctime")),
    })
}

/// 直播回放列表（1~2 请求；空列表是 2023 年后的平台常态，记 empty 不报错）。
/// 返回 (payload_value, 回放条目, 请求数)。
fn collect_live_records(
    client: &mut BilibiliClient,
    config: &Config,
) -> Result<(Value, Vec<Value>, i64), CollectError> {
    let started_requests = client.request_count() as i64;
    let (records, status, errors) =
        match client.live_records(&config.bilibili.room_id, RECORD_LIST_PAGE_SIZE) {
            Ok(items) => {
                let status = if items.is_empty() { "empty" } else { "ok" };
                (items, status, Vec::<String>::new())
            }
            Err(err) => (Vec::new(), "error", vec![brief(&err, 100)]),
        };
    let payload = json!({
        "platform": "bilibili",
        "captured_at": now_iso(),
        "room_id": config.bilibili.room_id,
        "status": status,
        "request_count": client.request_count() as i64 - started_requests,
        "count": records.len() as i64,
        "errors": errors,
        "records": records,
    });
    storage::write_json(
        &config.output_dir.join("shared").join("live_records.json"),
        &payload,
    )
    .map_err(CollectError::Storage)?;
    let used = payload["request_count"].as_i64().unwrap_or(0);
    let rows = payload["records"].as_array().cloned().unwrap_or_default();
    Ok((payload, rows, used))
}

/// 回放弹幕（rid 全分片；按场隔离错误；limit=live_replay_danmaku_limit 场数上限）。
/// 返回 (payload_value, 请求数, 弹幕行数)。
fn collect_replay_danmaku(
    client: &mut BilibiliClient,
    config: &Config,
    records: &[Value],
) -> Result<(Value, i64, i64), CollectError> {
    let limit = config.collection.live_replay_danmaku_limit;
    let started_requests = client.request_count() as i64;
    let mut bundles: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut line_total = 0;
    for record in records.iter().take(limit.max(0) as usize) {
        let rid = pystr(record.get("rid"));
        if rid.is_empty() {
            continue;
        }
        match client.live_record_danmaku(&rid) {
            Ok(messages) => {
                line_total += messages.len() as i64;
                bundles.push(json!({
                    "rid": rid,
                    "title": pystr(record.get("title")),
                    "area_name": pystr(record.get("area_name")),
                    "start_timestamp": record.get("start_timestamp").cloned().unwrap_or(Value::Null),
                    "end_timestamp": record.get("end_timestamp").cloned().unwrap_or(Value::Null),
                    "message_count": messages.len() as i64,
                    "messages": messages,
                }));
            }
            Err(err) => errors.push(format!("{rid}: {}", brief(&err, 100))),
        }
    }
    let status = if !bundles.is_empty() && errors.is_empty() {
        "ok"
    } else if !bundles.is_empty() {
        "partial"
    } else if !errors.is_empty() {
        "error"
    } else {
        "empty"
    };
    let payload = json!({
        "platform": "bilibili",
        "captured_at": now_iso(),
        "room_id": config.bilibili.room_id,
        "status": status,
        "limit": limit,
        "request_count": client.request_count() as i64 - started_requests,
        "record_count": bundles.len() as i64,
        "line_count": line_total,
        "errors": errors,
        "records": bundles,
    });
    storage::write_json(
        &config.output_dir.join("shared").join("replay_danmaku.json"),
        &payload,
    )
    .map_err(CollectError::Storage)?;
    let used = payload["request_count"].as_i64().unwrap_or(0);
    Ok((payload, used, line_total))
}

fn bump_counter(counter: &mut BTreeMap<String, i64>, key: String) {
    *counter.entry(key).or_insert(0) += 1;
}

fn collect_inner(
    client: &mut BilibiliClient,
    config: &Config,
    mode: &CollectMode,
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

    // 模式化种子名单：streamer-only 空（冷启动）、guards 名单、单查一人
    let guards: Vec<Value> = match mode {
        CollectMode::Guards => {
            emit("[2/5] 获取大航海与手工指定观众");
            client.guard_members(
                &config.bilibili.room_id,
                &config.bilibili.streamer_uid,
                config.collection.max_guards,
            )?
        }
        _ => Vec::new(),
    };
    let mut seeds: Vec<(String, Value)> = Vec::new();
    if let CollectMode::SingleViewer(uid) = mode {
        emit(&format!("[2/5] 单查观众：{uid}"));
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
    for uid in match mode {
        CollectMode::Guards => &config.bilibili.additional_viewer_ids[..],
        _ => &[][..],
    } {
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
    if seeds.is_empty() && matches!(mode, CollectMode::Guards | CollectMode::SingleViewer(_)) {
        return Err(CollectError::Message("no usable viewers were found".into()));
    }
    if !matches!(mode, CollectMode::StreamerOnly) {
        emit(&format!("[2/5] 观众种子：{} 人", seeds.len()));
    }

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

    // 房间级语料（M2-B2c：评论区浅存在 + 回放列表/弹幕，独立记账，不进深采池）
    emit("[5/5] 房间级语料：评论区浅存在 + 回放列表/弹幕");
    let cit = collect_room_comments(client, config)?;
    let records = collect_live_records(client, config)?;
    let danmaku = collect_replay_danmaku(client, config, &records.1)?;
    let coverage = json!({
        "video_comment_requests": cit.1,
        "video_comment_items": cit.2,
        "live_record_requests": records.2,
        "live_records": records.1.len() as i64,
        "replay_danmaku_requests": danmaku.1,
        "replay_danmaku_lines": danmaku.2,
    });
    emit(&format!(
        "覆盖：评论 {} 条/{} 请求，回放 {} 场，弹幕 {} 行/{} 请求",
        cit.2,
        cit.1,
        records.1.len(),
        danmaku.2,
        danmaku.1,
    ));

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
        "coverage": coverage,
        "started_at": started_at,
        "finished_at": now_iso(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn or_chain_treats_numeric_zero_as_falsy() {
        let zero = json!(0);
        let name = json!("名称");
        let empty = json!("");
        let bull = json!(false);
        assert_eq!(or_chain(&[&zero, &name]), "名称");
        assert_eq!(or_chain(&[&bull, &empty, &zero]), "");
        assert_eq!(or_chain(&[&json!(2.0), &name]), "2.0"); // Python str(2.0)="2.0"
    }

    #[test]
    fn platform_snapshot_drops_unobserved_zero_category() {
        let tmp = tempfile::tempdir().unwrap();
        let metadata = json!({
            "BVx": {"category": {"id": 0, "name": "", "parent_id": 0, "v2_name": ""}, "tags": []},
            "BVy": {"category": {"id": 167, "name": "知识", "parent_id": 0, "v2_name": ""}, "tags": ["t1"]},
        });
        let snapshot = write_platform_snapshot(tmp.path(), &metadata, Vec::new()).unwrap();
        let categories = snapshot["observed_video_categories"].as_array().unwrap();
        assert_eq!(categories.len(), 1, "id=0+空名分区必须被丢弃（未观测事实）");
        assert_eq!(categories[0]["id"], 167);
    }

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
