//! collect() 编排（移植 Python `collector.py` 的 `_collect_viewer / _enrich_video_metadata /
//! _write_platform_snapshot / _collect_streamer / collect`）。
//!
//! 预算纪律（与 Python 逐行对齐）：
//! - profile / relation_stat 手工特判（不走 `_call_source`）。
//! - followings/videos/dynamics/bangumi/games 走 `call_source`（1 请求/源）。
//! - favorites 嵌套预算：folders 列表 1 请求 + 每个公开收藏夹 items 各 1 请求，逐次记账。
//! - 错误隔离：单源失败只落 `source.status`（hidden/error），不中断观众与整体采集
//!   （失败只影响当前工作单元，AGENTS.md 哲学核心）。

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::bilibili::{BilibiliClient, BilibiliError};
use crate::config::Config;
use crate::episodes::now_iso;
use crate::storage;

pub(crate) mod enrich;
// MXA-9（r5-F1）：crate 内私有，与兄弟模块同纪律——外部消费者零（M5 CLI 需要时再开）。
pub(crate) mod leads;
pub(crate) mod room;
pub(crate) mod viewer;

use leads::{consume_approved_leads, fetch_lead_yield};

pub(crate) use enrich::*;

/// W1/r2-F1 文件名 uid 白名单字符集（与 live-server `uid_charset_legal` 同集，
/// 这里独立成一条是因为 live-core 不依赖 live-server；两实现同源记事于本头注）。
/// 单笔工作单元合法性阈值：几乎不会变的常理上界 128（大于 B 站现 uid 域与一个身位）。
pub(crate) fn uid_file_name_legal(uid: &str) -> bool {
    const MAX_UID_FILE_NAME_CHARS: usize = 128;
    !uid.is_empty()
        && uid.len() <= MAX_UID_FILE_NAME_CHARS
        && uid
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}
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
    /// 单查按钮：只深采指定 uid（seed_source=manual）——不清场，其余观众事实保留。
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
    // 险口 #1：单查（SingleViewer）不清场不归档——只覆写目标 uid 的
    // viewers/<uid>.json；其余舰长事实、site、shared 全原样。其余模式
    // （StreamerOnly/Guards）保持原清场+归档行为一字不差。
    // 用引用判断以免改动 `mode` 的所有权流；R2 起同时把这 bool 复用于
    // F1 的 /success/failure collection 收口与 F3 的线索消费闸。
    let single_viewer = matches!(&mode, CollectMode::SingleViewer(_));
    if !single_viewer {
        if config.perception.preserve_raw_snapshots
            && let Some(archived) =
                storage::archive_current_snapshot(&root).map_err(CollectError::Storage)?
        {
            emit(&format!("[0/5] 已归档上一轮快照：{}", archived.display()));
        }
        storage::reset_output(&root).map_err(CollectError::Storage)?;
    }
    let started_at = now_iso();
    let started = Instant::now();
    // 险口 #2（R2-F1）：单查不预写 "running" 态——中途失败/被杀不得把上一轮
    // collection.json（哪怕是 complete）覆写掉；单查的 collection 只在本轮成功
    // 收敛时落盘，失败即保持原字节或保持没有。其余模式保持原 running 预写。
    if !single_viewer {
        storage::write_json(
            &root.join("collection.json"),
            &json!({"status": "running", "started_at": started_at}),
        )
        .map_err(CollectError::Storage)?;
    }

    match collect_inner(&mut client, config, &mode, emit, &started_at, started) {
        Ok(mut summary) => {
            // M4.x kickoff D5/D6：尾段消费账本（预算 0 秒返=默认休眠；消费失败
            // 不杀 collection（薄切 fail-open 亲属的对应面）。
            // R2-F3：单查（SingleViewer）不消费已批准线索——线索消费是把当前
            // 账本已批准行整体烧尽的 batch 动作，单查是 single-point 增量检查，
            // 不该动账本（lead_fetch_budget_per_run > 0 也不放行）。
            let consumed = if single_viewer {
                0
            } else {
                // G2-B 自治 L1：消费前先把谓词合格的 pending 行自动迁 approved
                //（autonomy=0 → 秒返 0，L0 现状纯人工一字不动；R2-F3 单查同样
                // 不进此闸——单查不动账本是闸门级纪律）。
                let auto = leads::auto_approve_pending_leads(
                    &root,
                    &room_roster(&root, config),
                    config.collection.leads_autonomy,
                    emit,
                );
                if auto > 0 {
                    emit(&format!("[LEADS] L1 自动批准 {auto} 条待审线索"));
                }
                consume_approved_leads(
                    &root,
                    config.collection.lead_fetch_budget_per_run,
                    &mut |row| fetch_lead_yield(&mut client, row),
                    emit,
                )
            };
            if consumed > 0 {
                emit(&format!("[LEADS] 本轮消费 {consumed} 条已批准线索"));
                // M4.x-T1 冻结：三态 schema = {缺席, 正整数}；零消费不面世，
                // 面板/报告消费者按「显式 i64、缺省 0」解读。
                summary["leads_consumed"] = json!(consumed);
            }
            // MXA-2（r3-F1）：request_count 在 collect_inner 内冻结——消费请求真实
            // 发生，写盘前刷新不漏报（否则传染 baseline 的 collection_request_count）。
            summary["request_count"] = json!(client.request_count());
            if single_viewer {
                // R2-F1 成功收口：单查写 collection.json 但口径诚实——status 固定
                // complete；viewer_count = 盘面 viewers/*.json 实际文件数（含未动手
                // 他人），不再落成 seed=1 的空壳骗过项目概览；guard_count 读取既有
                // summary 值（冷启动读不到置 0）；request_count/elapsed_seconds 等
                // 本轮采集指标照旧记本轮。summary 亦同对象，返回给调用方。
                summary["status"] = json!("complete");
                summary["viewer_count"] = json!(viewer_json_file_count(&root)?);
                let guard_count = storage::read_json(&root.join("collection.json"))
                    .ok()
                    .flatten()
                    .and_then(|value| value.get("guard_count").cloned())
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                summary["guard_count"] = json!(guard_count);
            }
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
            // R2-F1 失败收口：单查失败完全不写 collection.json——上一轮字节（哪怕
            // 是 complete）原样保留，失败不杀伤集合门禁（episodes/baseline.rs 只认
            // status=="complete"）；冷启动本来就没 collection.json 就保持没有。
            // 其余模式保持原 failed 文案一字不差。
            if !single_viewer
                && storage::write_json(
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

/// G2-B：本房间既有名册 = viewers/*.json 文件名 ∪ 主播 uid——L1 自动批准的
/// creator 谓词查据（采已在册者零增量）。目录缺席 = 仅主播 uid（冷启动态）。
fn room_roster(root: &std::path::Path, config: &Config) -> BTreeSet<String> {
    let mut roster = BTreeSet::from([config.bilibili.streamer_uid.clone()]);
    if let Ok(entries) = std::fs::read_dir(root.join("viewers")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                roster.insert(stem.to_string());
            }
        }
    }
    roster
}

/// R2-F1：单查成功收口的盘面口径——viewers/*.json 实际文件数（含未动他人）。
fn viewer_json_file_count(root: &std::path::Path) -> Result<i64, CollectError> {
    let directory = root.join("viewers");
    if !directory.is_dir() {
        return Ok(0);
    }
    let entries = std::fs::read_dir(&directory)
        .map_err(|err| CollectError::Storage(format!("read_dir {}: {err}", directory.display())))?;
    let mut count = 0_i64;
    for entry in entries {
        let entry = entry.map_err(|err| {
            CollectError::Storage(format!("read_dir entry {}: {err}", directory.display()))
        })?;
        if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
            count += 1;
        }
    }
    Ok(count)
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
    // R2-F2：SingleViewer 的显式目标 uid——后续 enrich 收面用它过滤 viewer 文件，
    // 避免无差别全量回写其余舰长（每次单查都翻一批 input_hash → Z5「重保 AI」破）。
    let single_viewer_uid: Option<String> = if let CollectMode::SingleViewer(uid) = mode {
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
        Some(uid)
    } else {
        None
    };
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
        // W1/r2-F1 纵深防御：落盘文件名 uid 必须是白名单字符集 [A-Za-z0-9_-]，
        // 否则拒绝本单元（即便 server 层已拦截，collect 本身不得裸写穿透）。
        if !uid_file_name_legal(&uid) {
            return Err(CollectError::Message(format!(
                "viewer id 非法（限 [A-Za-z0-9_-]）：{uid:?}"
            )));
        }
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
        single_viewer_uid.as_deref(),
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

    /// W1/r2-F1 文件名守卫钉：落盘 uid 只许白名单字符集——
    /// dot-dot/内嵌斜杠/空串/超长/非 ASCII 一律拒（与 live-server uid_charset_legal 同族）。
    #[test]
    fn uid_file_name_legal_rejects_path_traversal() {
        assert!(uid_file_name_legal("1003"));
        assert!(uid_file_name_legal("demo-123_test"));
        assert!(!uid_file_name_legal(".."));
        assert!(!uid_file_name_legal("../escape"));
        assert!(!uid_file_name_legal("a/b"));
        assert!(!uid_file_name_legal("USP-中"));
        assert!(!uid_file_name_legal(""));
        assert!(!uid_file_name_legal(&"x".repeat(129)));
    }
}
