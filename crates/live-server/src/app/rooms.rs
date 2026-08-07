//! 房间数据面：overview / viewers / tree + leads 审批缝（B2/G2-B）。
//!
//! 自 `app.rs` 按头注 rooms/config/runs 条款拆出——共享面
//! （fail/AppResult/load_config/data_root/守卫/open_graph）留根卷，路径零变化。

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{
    AppResult, AppState, MAX_VID_PATH_CHARS, data_root, fail, internal, load_config, open_graph,
    room_guard, uid_charset_legal, vid_guard,
};

/// 读面统一走 live_core::storage：文件缺席 = None（合法空态）；
/// 文件在但 JSON 损坏 = 响亮 eprintln 后仍按 None 空态处理——九个调用点全是
/// 「读到就用、读不到就空态」语义，静默吞损坏违反 AGENTS.md，故在收窄点报响。
fn read_json(path: &std::path::Path) -> Option<Value> {
    match live_core::storage::read_json(path) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("读取 JSON 失败（按空态处理）：{err}");
            None
        }
    }
}

pub(super) async fn rooms_list(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    // 现布局单房间——uid 暂恒等于 config 房号（路径形是设计承诺，非多房间实现）。
    Ok(Json(json!([{
        "id": config.bilibili.room_id,
        "project_name": config.project_name,
        "streamer_uid": config.bilibili.streamer_uid,
        "output_dir": config.output_dir.display().to_string(),
    }])))
}

// ---------------------------------------------------------------------------
// 房间数据面（B2）：overview / viewers / tree
// ---------------------------------------------------------------------------

/// 无图态的 delta 形状（与 live_core::graph::query::run_pair_delta 的 baseline 臂同形）。
const BASELINE_DELTA: &str = r#"{"baseline_only":true,"from_run_id":null,"to_run_id":null,"interest":{"opened":[],"closed":[],"changed":[]},"guards":{"added":[],"removed":[]}}"#;

pub(super) async fn room_overview(
    State(state): State<AppState>,
    Path(uid): Path<String>,
) -> AppResult<Json<Value>> {
    // 本体全同步（config/文件读 + rusqlite 三路 + 投影）——
    // 收进 spawn_blocking，与 compute_graph_bytes/ensure_graph_artifact 同款姿态，
    // 不再卡 tokio executor。
    tokio::task::spawn_blocking(move || room_overview_blocking(&state, &uid))
        .await
        .map_err(internal)?
}

fn room_overview_blocking(state: &AppState, uid: &str) -> AppResult<Json<Value>> {
    let config = load_config(state)?;
    room_guard(&config, uid)?;
    let root = data_root(state)?;
    let Some(mut collection) = read_json(&root.join("collection.json")) else {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "collection.json 尚未存在——请先触发一次运行",
        ));
    };
    // M4.x-T1 schema 冻结的读取侧：显式 i64，缺省 0。
    if collection.get("leads_consumed").is_none() {
        collection["leads_consumed"] = json!(0);
    }
    let store = open_graph(&root);
    // 表形态（design §9.2 行 254）：leads 读面唯一源 = discovery_leads 表。
    // 无库 → 空集合（一次性 JSONL 迁移已随删码刀5 退役）。
    let rows = match &store {
        Some(store) => live_core::leads::read_rows(store).map_err(internal)?,
        None => Vec::new(),
    };
    let count =
        |status: live_core::leads::LeadStatus| rows.iter().filter(|r| r.status == status).count();
    let delta = match &store {
        Some(store) => live_core::graph::query::run_pair_delta(store).map_err(internal)?,
        None => serde_json::from_str(BASELINE_DELTA).expect("literal parses"),
    };
    // 首页指标条：无图态 → null 空态（前端呈现「—」而非臆造数字）。
    let graph_stats = match &store {
        Some(store) => Some(live_core::graph::query::graph_stats(store).map_err(internal)?),
        None => None,
    };
    // BriefingCard refs 可点的归属解析面——episode_id → {viewer_id, title, source}。
    // 轻量投影（GRAPH_QUERY_LIMIT=500 帽，超出部分 ref 落未解析态、chip 不可点），
    // 不抄整行（fields/platform_facts 大键留在 tree/graph 端点）。
    // source 同行透出——芯片副文本 = Episode 类型词（动态/投稿/弹幕…），
    // 不再拿标题当持有人。
    let episode_index: Value = match &store {
        Some(store) => {
            let rows = live_core::graph::query::episodes(store, "", None).map_err(internal)?;
            let map = rows
                .iter()
                .filter_map(|row| {
                    let episode_id = row["episode_id"].as_str()?;
                    let viewer_id = row["viewer_id"].as_str()?;
                    Some((
                        episode_id.to_string(),
                        json!({"viewer_id": viewer_id, "title": row["title"], "source": row["source"]}),
                    ))
                })
                .collect::<serde_json::Map<String, Value>>();
            Value::Object(map)
        }
        None => json!({}),
    };
    Ok(Json(json!({
        "room_id": config.bilibili.room_id,
        "streamer_uid": config.bilibili.streamer_uid,
        "project_name": config.project_name,
        // 主播卡数据面（主页签名）：streamer.json 的 profile 段原样透传——
        // sources.videos 属事实层原料且体大，不上 overview 面；缺文件 → null 空态。
        "streamer": read_json(&root.join("streamer.json"))
            .and_then(|v| v.get("profile").cloned()),
        // 直播数据页档案面：shared/live_records.json 整场记录原样透传
        //（status/count/records[]；空态 status="empty" 由前端解说）。
        "live": read_json(&root.join("shared").join("live_records.json")),
        // 图存量指标面（旧版报告顶部数字条）。
        "graph_stats": graph_stats,
        //（迭代细则 v1 §1）下播复盘卡——pipeline 落盘的 ai/recap.json
        // 原样透传（四纯规则数 + AI 命名件 + 未知行）。缺文件 → null：前端呈现
        // 「复盘尚未生成」而非臆造（同一零猜测纪律）。
        "recap": read_json(&root.join("ai").join("recap.json")),
        // BriefingCard ref → 归属观众树页的解析索引（无图态 → {} 空态）。
        "episode_index": episode_index,
        "collection": collection,
        "ai": read_json(&root.join("ai").join("state.json")),
        // situation 保留（deprecated）= BriefingCard 的 front_brief 数据源
        // （API 兼容）；「态势项」胶囊/宏观折叠组等前台直呈已整段退役——勿新挂直呈消费。
        "situation": read_json(&root.join("ai").join("situation.json")),
        "leads": {
            // summary 键零消费者（FE 不渲、真消费者是 pipeline annex）——砍。
            "totals": {
                "pending_approval": count(live_core::leads::LeadStatus::PendingApproval),
                "approved": count(live_core::leads::LeadStatus::Approved),
                "consumed": count(live_core::leads::LeadStatus::Consumed),
                "rejected": count(live_core::leads::LeadStatus::Rejected),
            },
            // 人工审批面：pending 明细直出（前端列表渲染；写账序直出，
            // 「响度按 viewer 分组」未做——高层待阅时再议排序，G2-F4 裁向在案）
            "pending": rows.iter()
                .filter(|r| r.status == live_core::leads::LeadStatus::PendingApproval)
                .collect::<Vec<_>>(),
            // rejected 明细直出（含拒因留档；前端 rejected 徽标展开可回看
            // 记录的 note——只读事实面，绝不代行裁决）。
            "rejected": rows.iter()
                .filter(|r| r.status == live_core::leads::LeadStatus::Rejected)
                .collect::<Vec<_>>(),
        },
        "delta": delta,
    })))
}

pub(super) async fn room_viewers(
    State(state): State<AppState>,
    Path(uid): Path<String>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    let root = data_root(&state)?;
    //「舰长卡→关系卡」四微件的数据口（graph 在才开库的只读面）。
    let store = open_graph(&root);
    let today_secs = live_core::episodes::now_unix_secs();
    let viewers_dir = root.join("viewers");
    let mut viewers: Vec<Value> = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&viewers_dir)
        .into_iter()
        .flat_map(|dirs| dirs.flatten())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    entries.sort();
    for path in entries {
        let Some(viewer) = read_json(&path) else {
            continue;
        };
        let uid = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        let cached = read_json(
            &root
                .join("ai")
                .join("perception")
                .join("viewers")
                .join(format!("{uid}.json")),
        );
        // 出勤面每行一查（COUNT DISTINCT + MAX 双聚合一路返回）。
        let presence = store
            .as_ref()
            .and_then(|s| live_core::graph::query::room_presence(s, &uid).ok());
        viewers.push(json!({
            "uid": uid,
            "name": viewer["viewer"]["name"].as_str()
                .or_else(|| viewer["profile"]["name"].as_str()),
            // 大航海 API 一发即带的身份面（旧版站的舰长签名：头像+舰长等级+勋章）——
            // face 是 hdslb 图床 URL，呈现侧须 referrerPolicy="no-referrer"。
            "face": viewer["viewer"]["face"].as_str()
                .or_else(|| viewer["profile"]["face"].as_str()),
            "guard_level": viewer["viewer"]["guard_level"].as_i64(),
            "medal_level": viewer["viewer"]["medal_level"].as_i64(),
            "collected_at": viewer["collected_at"],
            "ai_status": cached.as_ref().map(|c| c["status"].clone()),
            // 空池引导位约定：front-end 按 completed=false + viewer 数=0 渲染引导。
            "ai_completed": cached.as_ref().is_some_and(|c| c["status"] == "complete"),
            // 时效位：旧 AI 结论保留但信源已变 → 行面亮「信源已更新·待重判」。
            // null（无参考旧结论 / 非 complete）与 false（绿灯时效内）区分。
            "ai_stale": cached
                .as_ref()
                .and_then(|c| live_core::agent::pipeline::viewer_perception_stale(&config, &viewer, c)),
            // 四微件（缺件 = null，前端落「未知」微行，绝不补文案/编数字）：
            // ① 第几次来 = WS 场次窗到访计数（无库/无记录 → null ≠ 0 次）；
            "visit_count": presence
                .and_then(|(visits, _)| (visits > 0).then_some(visits)),
            // ② 距上次 N 天 = 末次 WS 到场的整日数（负数不下泄，挡未来 ts）。
            "days_since_last": presence
                .and_then(|(_, last_ts)| last_ts.map(|ts| ((today_secs - ts) / 86_400).max(0))),
            // ③ 身份一句 = AI 感知 profile_summary 首 40 字（AI 语义徽标由前端盖）。
            "identity_line": cached
                .as_ref()
                .filter(|c| c["status"] == "complete")
                .and_then(|c| c["analysis"]["profile_summary"].as_str())
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| line.chars().take(40).collect::<String>()),
            // ④ 最新动态日期 = 名下条目最新 published_at 的日期段（ISO 前 10 字符）。
            "latest_activity_date": store
                .as_ref()
                .and_then(|s| live_core::graph::query::latest_published_at(s, &uid).ok())
                .flatten()
                .map(|iso| iso.chars().take(10).collect::<String>()),
        }));
    }
    Ok(Json(json!(viewers)))
}

pub(super) async fn viewer_tree(
    State(state): State<AppState>,
    Path((uid, vid)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    vid_guard(&vid)?;
    let root = data_root(&state)?;
    let viewer_path = root.join("viewers").join(format!("{vid}.json"));
    let Some(viewer) = read_json(&viewer_path) else {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("观众 {vid} 没有采集资料于观众面（或尚未采集）"),
        ));
    };
    let (episodes, mentions) = match open_graph(&root) {
        Some(store) => (
            live_core::graph::query::episodes(&store, &vid, None).map_err(internal)?,
            live_core::graph::query::mentions_of_viewer(&store, &vid, None).map_err(internal)?,
        ),
        // 图尚未落盘 → 信息空面（viewer 原料 + ai 缓存仍可读，写盘前态）。
        None => (Vec::new(), Vec::new()),
    };
    let cached = read_json(
        &root
            .join("ai")
            .join("perception")
            .join("viewers")
            .join(format!("{vid}.json")),
    );
    // 时效位：与 room_viewers 行完全同源——cached 存在且 complete 才判哈希；
    // true=信源已更新待重判 / false=时效内绿灯 / null=无参考旧结论。
    let ai_stale = cached
        .as_ref()
        .and_then(|c| live_core::agent::pipeline::viewer_perception_stale(&config, &viewer, c));
    Ok(Json(json!({
        "uid": vid,
        "viewer": viewer,
        "ai": cached,
        "ai_stale": ai_stale,
        "episodes": episodes,
        "mentions": mentions,
    })))
}

// ---------------------------------------------------------------------------
// leads 审批缝（G2-B 工作项 1）：POST /api/rooms/:uid/leads/:lead_id/approve
// ---------------------------------------------------------------------------

/// `lead_id` = 账本行 `dedupe_key`（身份：`(type, locator)` 的 hash；16hex）。
///
/// G2 表形态（design §9.2 行 254）：账面 = discovery_leads 表；先一次性把旧
/// JSONL 账本入库归档（幂等迁移的写面触点），随后全链路只碰表。
/// 状态机单行道 `pending_approval → approved`（live_core::leads::approve_transition
/// 唯一裁决点）：
/// - 正常翻转：读行 → 改状态 → `update_lead_row` 受控落库；
/// - 幂等重放：已 approved → 200 相同终态，表行不动；
/// - 不存在（lead_id 未知 / 房间错 / 穿透形 id）→ 404（统一错误形态）；
/// - 非法迁移（consumed/rejected/deferred 源态）→ 422，错文讲规则 + 当前状态；
/// - 账本迁移守卫失败（旧 JSONL 含坏行）→ 500 响铃，绝不带病写。
pub(super) async fn lead_approve(
    State(state): State<AppState>,
    Path((uid, lead_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    // 路径消毒与 viewer uid 同口径（%2F 解码后的穿透形在此截断，404 与不存在同形）。
    if !uid_charset_legal(&lead_id, MAX_VID_PATH_CHARS) {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("lead {lead_id} 不存在"),
        ));
    }
    let root = data_root(&state)?;
    // 写面端点：图库缺席则建仓（首触即 v7 schema；与纯读路径的「不建文件」纪律分野）。
    let store_path = root.join("graph").join("perception.sqlite3");
    let store = live_core::graph::store::Store::open(&store_path).map_err(internal)?;
    let Some(mut row) = store.lead_row(&lead_id).map_err(internal)? else {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("lead {lead_id} 不存在"),
        ));
    };
    let changed = live_core::leads::approve_transition(row.status)
        .map_err(|message| fail(StatusCode::UNPROCESSABLE_ENTITY, &message))?;
    if changed {
        row.status = live_core::leads::LeadStatus::Approved;
        store.update_lead_row(&row).map_err(internal)?;
    }
    Ok(Json(json!({
        "dedupe_key": row.dedupe_key,
        "status": live_core::leads::status_name(row.status),
        // 为幂等重放留可观测位：终态（dedupe_key/status）恒定，仅动作位分野。
        "changed": changed,
    })))
}

// ---------------------------------------------------------------------------
// leads 拒绝缝：POST /api/rooms/:uid/leads/:lead_id/reject
// ---------------------------------------------------------------------------

/// `lead_id` = 账本行 `dedupe_key`（与 approve 同身份口径）。
///
/// 状态机单行道 `pending_approval → rejected`（reject_transition 唯一裁决点）：
/// - 正常翻转：读行 → 拒因规范化（reason trim + ≤REJECT_NOTE_CAP 字）→ 改状态 +
///   reject_note 受控落库（空 reason = 全空合法，落 NULL）；
/// - 幂等重放：已 rejected → 200 相同终态，表行不动（新携 reason 不覆盖留档——
///   改判先谈人，不靠端点打架）；
/// - 不存在（lead_id 未知 / 房间错 / 穿透形 id）→ 404（统一错误形态）；
/// - 非法迁移（consumed/approved 源态）→ 422，错文讲规则 + 当前状态。
pub(super) async fn lead_reject(
    State(state): State<AppState>,
    Path((uid, lead_id)): Path<(String, String)>,
    // 体可选——None / 空体 = 空拒因（合法）。不套 Option<JsonBody<T>>：
    // 它会把坏 JSON 吞成 None（静默放过坏参）；这里拿原始字节自行判别——
    // 空体空参放行，坏 JSON 显式 422（规格外自裁）。
    body: Option<axum::body::Bytes>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    // 路径消毒与 viewer uid 同口径（%2F 解码后的穿透形在此截断，404 与不存在同形）。
    if !uid_charset_legal(&lead_id, MAX_VID_PATH_CHARS) {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("lead {lead_id} 不存在"),
        ));
    }
    let reason = parse_reject_reason(body)?;
    let root = data_root(&state)?;
    // 写面端点：图库缺席则建仓（与 approve 同纪律）。
    let store_path = root.join("graph").join("perception.sqlite3");
    let store = live_core::graph::store::Store::open(&store_path).map_err(internal)?;
    let Some(mut row) = store.lead_row(&lead_id).map_err(internal)? else {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("lead {lead_id} 不存在"),
        ));
    };
    let changed = live_core::leads::reject_transition(row.status)
        .map_err(|message| fail(StatusCode::UNPROCESSABLE_ENTITY, &message))?;
    if changed {
        row.status = live_core::leads::LeadStatus::Rejected;
        row.reject_note = reason;
        store.update_lead_row(&row).map_err(internal)?;
    }
    Ok(Json(json!({
        "dedupe_key": row.dedupe_key,
        "status": live_core::leads::status_name(row.status),
        "changed": changed,
        "reject_note": row.reject_note,
    })))
}

/// reject 体解析——体缺省/空体 → 空拒因；坏 JSON/非对象 → 422；
/// `reason` 须字符串（trim 后 ≤REJECT_NOTE_CAP 字，空串合法）。
fn parse_reject_reason(body: Option<axum::body::Bytes>) -> AppResult<String> {
    let Some(bytes) = body else {
        return Ok(String::new());
    };
    if bytes.is_empty() {
        return Ok(String::new());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
        fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("reject 体不是合法 JSON 对象：{err}"),
        )
    })?;
    let Some(object) = value.as_object() else {
        return Err(fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "reject 体必须是 JSON 对象（{\"reason\": \"...\"}）",
        ));
    };
    let reason = match object.get("reason") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.trim().to_string(),
        Some(_) => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "reject 体的 reason 必须是字符串",
            ));
        }
    };
    if reason.chars().count() > live_core::leads::REJECT_NOTE_CAP {
        return Err(fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!(
                "拒因注记最多 {cap} 字",
                cap = live_core::leads::REJECT_NOTE_CAP
            ),
        ));
    }
    Ok(reason)
}
