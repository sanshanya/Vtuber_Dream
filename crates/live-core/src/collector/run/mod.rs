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
use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::bilibili::{BilibiliClient, BilibiliError};
use crate::config::Config;
use crate::episodes::now_iso;
use crate::storage;

pub(crate) mod enrich;
pub(crate) mod room;
pub(crate) mod viewer;

pub(crate) use enrich::*;
pub(crate) use room::*;
pub(crate) use viewer::*;

use super::brief;

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
            if storage::write_json(
                &root.join("collection.json"),
                &json!({
                    "status": "failed",
                    "started_at": started_at,
                    "updated_at": now_iso(),
                    "detail": err.to_string(),
                }),
            )
            .is_err()
            {
                emit("警告：collection.json 失败状态写盘失败");
            }
            Err(err)
        }
    }
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
        let uid = uid.trim().to_string();
        if uid.is_empty() {
            return Err(CollectError::Message(
                "single-viewer mode requires a non-empty uid".into(),
            ));
        }
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
                *source_counter.entry(key).or_insert(0) += 1;
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

    emit("[5/5] 采集主播近期内容 + 房间级语料");
    let streamer = collect_streamer(client, config, &metadata_cache);
    storage::write_json(&config.output_dir.join("streamer.json"), &streamer)
        .map_err(CollectError::Storage)?;

    // 房间级语料（M2-B2c：评论区浅存在 + 回放列表/弹幕，独立记账，不进深采池）
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
}
