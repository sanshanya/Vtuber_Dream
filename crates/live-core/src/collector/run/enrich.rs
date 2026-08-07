//! enrich：观察到的视频回灌 TAG/分区 + 平台快照（Python `_enrich_video_metadata`/`_write_platform_snapshot`/`_collect_streamer`）。

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::bilibili::BilibiliClient;
use crate::config::Config;
use crate::episodes::now_iso;
use crate::storage;

use super::super::{brief, normalize_dynamics, normalize_profile, normalize_videos};
use super::{CollectError, or_chain, py_int, pystr};

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
/// Python priority.get(source, 9) 的默认哨兵（未知来源永远排最后）。
const ENRICH_PRIORITY_FALLBACK: i64 = 9;

/// SingleViewer 模式的收面参数。`target_uid` 为 Some 时只处理与该 uid
/// 相符的 viewer 文件（候选 bvid 与回写全收窄到目标单点），None = 全量照旧。
pub fn enrich_video_metadata(
    client: &mut BilibiliClient,
    root: &Path,
    limit: i64,
    target_uid: Option<&str>,
    emit: &mut dyn FnMut(&str),
) -> Result<Value, CollectError> {
    // 单查只吸收目标 uid 的 viewer 文件进 enrich——其余舰长（带 sources 的
    // 旧壳）不参与候选、更不参与回写；否则每次单查都会给一堆旧舰长补 tags/
    // platform_category → input_hash 全翻 → 大批 AI 重跑（「重保 AI」语义破）。
    // 房间级 shared（platform_snapshot/streamer/room_comments/live_records/
    // replay_danmaku）仍照常刷新——已把 captured_at 摘出 audience hash 件，
    // 同内容重采不翻脸；且单查→audience 链需要 baseline 原料（本轮 collection
    // complete 门禁 + shared 语料），这些刷新属供给而非翻脸。
    let mut viewers = storage::load_viewers(root).map_err(CollectError::Storage)?;
    if let Some(uid) = target_uid {
        viewers.retain(|viewer| pystr(viewer.pointer("/viewer/id")).trim() == uid);
    }
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
                    .unwrap_or(ENRICH_PRIORITY_FALLBACK);
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

    // 回写 viewers（viewers 已在上方按 target_uid 收窄；Python：逐文件、变了才写）
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
