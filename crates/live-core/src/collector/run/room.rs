//! 房间级浅存在采集点（评论区 / 回放列表 / 回放弹幕）。

use serde_json::{Value, json};

use crate::bilibili::BilibiliClient;
use crate::config::Config;
use crate::episodes::now_iso;
use crate::storage;

use super::super::{brief, content_id};
use super::{CollectError, pystr};

// ---------------------------------------------------------------------------
// 房间级浅存在采集点（design M2-B2c；每个点独立记账，失败隔离，不进深采池）
// ---------------------------------------------------------------------------

const DISCOVERY_LIMIT: i64 = 3;
const ROOM_COMMENT_PAGE_SIZE: i64 = 20;
const RECORD_LIST_PAGE_SIZE: i64 = 20;

/// 评论区浅存在（type=1 视频 oid=avid、type=17 动态 oid=动态数字 id；第一页，无 wbi）。
/// 返回 (payload_value, 请求数, 评论行数)。
pub fn collect_room_comments(
    client: &mut BilibiliClient,
    config: &Config,
) -> Result<(Value, i64, i64), CollectError> {
    let budget = config.collection.room_comment_request_budget;
    let started_requests = client.request_count() as i64;
    // budget<=0 = 真关闭（与 live_replay_danmaku_limit=0 对称）：零请求、零文件内容。
    if budget <= 0 {
        let payload = json!({
            "platform": "bilibili",
            "captured_at": now_iso(),
            "streamer_uid": config.bilibili.streamer_uid,
            "status": "disabled",
            "budget": budget,
            "targets": [],
            "request_count": 0,
            "count": 0,
            "errors": [],
            "rows": [],
        });
        storage::write_json(
            &config.output_dir.join("shared").join("room_comments.json"),
            &payload,
        )
        .map_err(CollectError::Storage)?;
        return Ok((payload, 0, 0));
    }
    let uid = &config.bilibili.streamer_uid;
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
                    if let Some(row) =
                        normalize_comment(&config.bilibili.room_id, *type_id, oid, item)
                    {
                        rows.push(row);
                    }
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
    let request_delta = client.request_count() as i64 - started_requests;
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
        "request_count": request_delta,
        "count": rows.len() as i64,
        "errors": errors,
        "rows": rows,
    });
    storage::write_json(
        &config.output_dir.join("shared").join("room_comments.json"),
        &payload,
    )
    .map_err(CollectError::Storage)?;
    let count = payload["count"].as_i64().unwrap_or(0);
    Ok((payload, request_delta, count))
}

/// 轮2-R1-B：返回 Option——rpid 与正文双空的行没有幂等身份
/// （content_id("","") 会把跨目标垃圾行塌缩成同 id 互相覆盖），宁缺毋滥直接拒收。
fn normalize_comment(room_id: &str, type_id: i64, oid: &str, item: &Value) -> Option<Value> {
    let kind = if type_id == 1 { "video" } else { "dynamic" };
    let owner = format!("room:{room_id}");
    let mut rpid = pystr(item.get("rpid_str"));
    if rpid.is_empty() {
        rpid = pystr(item.get("rpid"));
    }
    let member = item.get("member").cloned().unwrap_or(Value::Null);
    let message = pystr(item.pointer("/content/message"));
    if rpid.is_empty() && message.is_empty() {
        return None;
    }
    Some(json!({
        "id": content_id("comment", &owner, &rpid, &message),
        "source": "comment",
        "target_kind": kind,
        "target_oid": oid,
        "rpid": rpid,
        "mid": pystr(member.get("mid")),
        "uname": pystr(member.get("uname")),
        "message": message,
        "ctime": pystr(item.get("ctime")),
    }))
}

/// 直播回放列表（1~2 请求；空列表是 2023 年后的平台常态，记 empty 不报错）。
/// 返回 (payload_value, 回放条目, 请求数)。
pub fn collect_live_records(
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
    let request_delta = client.request_count() as i64 - started_requests;
    let payload = json!({
        "platform": "bilibili",
        "captured_at": now_iso(),
        "room_id": config.bilibili.room_id,
        "status": status,
        "request_count": request_delta,
        "count": records.len() as i64,
        "errors": errors,
        "records": records,
    });
    storage::write_json(
        &config.output_dir.join("shared").join("live_records.json"),
        &payload,
    )
    .map_err(CollectError::Storage)?;
    let rows = payload["records"].as_array().cloned().unwrap_or_default();
    Ok((payload, rows, request_delta))
}

/// 回放弹幕（rid 全分片；按场隔离错误；limit=live_replay_danmaku_limit 场数上限）。
/// 返回 (payload_value, 请求数, 弹幕行数)。
pub fn collect_replay_danmaku(
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
    // status 与其他采集点对称：有行才 ok、有行带错记 partial、全空记 empty、全无记 error。
    let status = if line_total > 0 && errors.is_empty() {
        "ok"
    } else if line_total > 0 {
        "partial"
    } else if bundles.is_empty() && !errors.is_empty() {
        "error"
    } else if !bundles.is_empty() {
        // 有回放记录但 0 行弹幕（例如小房间旧回放）——"空内容"可观测
        "empty"
    } else if !errors.is_empty() {
        "error"
    } else {
        "empty"
    };
    let request_delta = client.request_count() as i64 - started_requests;
    let payload = json!({
        "platform": "bilibili",
        "captured_at": now_iso(),
        "room_id": config.bilibili.room_id,
        "status": status,
        "limit": limit,
        "request_count": request_delta,
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
    Ok((payload, request_delta, line_total))
}
