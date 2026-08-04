//! axum 装配与端点（D1/D3/D9 + B1 切：rooms + config + 静态面）。
//!
//! 与安全面相干的所有魔数就地命名（kickoff 完成定义 5）。

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde_json::{Value, json};
use tower_http::services::ServeDir;

use crate::registry::Registry;

/// serve/run 共用默认端口（D5：魔数命名）。
pub const DEFAULT_PORT: u16 = 3781;
/// D9：任何 JSON 请求体的上限（POST runs / PUT config 共用口径）。
pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
/// D9：PUT 单值长度上限。
pub const MAX_PUT_VALUE_CHARS: usize = 4096;
/// D9：POST runs 的 viewer_uid 长度上限（B 站 uid 为数字串，32 已富余）。
pub const MAX_VIEWER_UID_CHARS: usize = 32;
/// D6：允许的写入键白名单（(顶层段, 键)）——此后扩展需要同名加键 + 测试。
pub const WRITABLE_CONFIG_KEYS: [(&str, &str); 4] = [
    ("bilibili", "cookie"),
    ("ai", "api_key"),
    ("ai", "base_url"),
    ("ai", "model"),
];

#[derive(Clone)]
pub struct AppState {
    pub config_path: PathBuf,
    /// 静态面根（D2：不内嵌；生产 cwd/web/dist、测试可注入）。
    pub web_root: PathBuf,
    pub registry: Registry,
    /// D5/G3：demo（run --demo）模式——run 通道返回静态快照，不触发真实运行。
    pub demo: bool,
    /// D5：数据呈现根覆盖 —— run --demo 让它指向 _demo，serve 常态 = None → 从 config 每请求读取。
    pub data_root: Option<PathBuf>,
    /// 测试接缝：POST runs → spawn 的 Bilibili 根地址注入（生产 None → 官方端点）。
    pub bilibili_hosts: Option<(String, String)>,
}

/// 统一错误包装记类型：状态码 + {"error": 文案}（D3 形态）。
///
/// 用记号类型而非裸 Response：`Result<Json, Response>` 中 Response 是 axum 的终止型
/// （boxed body），clippy result_large_err + Handler 约束联合下只能包一层。
pub struct AppFail(Box<Response>);

impl AppFail {
    fn new(status: StatusCode, message: &str) -> Self {
        Self(Box::new(
            (status, Json(json!({"error": message}))).into_response(),
        ))
    }
}

impl IntoResponse for AppFail {
    fn into_response(self) -> Response {
        *self.0
    }
}

fn fail(status: StatusCode, message: &str) -> AppFail {
    AppFail::new(status, message)
}

type AppResult<T> = Result<T, AppFail>;

fn load_config(state: &AppState) -> AppResult<live_core::config::Config> {
    live_core::config::load_config(&state.config_path)
        .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))
}

/// 数据根统一落点：demo 覆盖 → config.output_dir。
fn data_root(state: &AppState) -> AppResult<PathBuf> {
    match &state.data_root {
        Some(root) => Ok(root.clone()),
        None => Ok(load_config(state)?.output_dir),
    }
}

fn fail_box(status: StatusCode, message: &str) -> AppFail {
    AppFail::new(status, message)
}

/// D9/ag3-F4：JSON 体信封化——所有 JsonRejection（含 DefaultBodyLimit 触发的 413）
/// 统一落成 {error} JSON 响应，保持 D3 错误形态；原生 axum 只会吐裸文本。
struct JsonBody<T>(T);

#[axum::async_trait]
impl<S, T> axum::extract::FromRequest<S> for JsonBody<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = AppFail;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => {
                let status = rejection.status();
                Err(AppFail::new(status, &rejection.body_text()))
            }
        }
    }
}

pub fn build_app(state: AppState) -> Router {
    let api = Router::new()
        .route("/rooms", get(rooms_list))
        .route("/rooms/:uid/overview", get(room_overview))
        .route("/rooms/:uid/viewers", get(room_viewers))
        .route("/rooms/:uid/viewers/:vid/tree", get(viewer_tree))
        .route("/rooms/:uid/viewers/:vid/graph", get(viewer_graph))
        .route("/rooms/:uid/graph", get(room_graph))
        .route("/config", get(config_get).put(config_put))
        .route("/runs", axum::routing::post(runs_post))
        .route("/runs/:id", get(run_get))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state.clone());

    let router = Router::new().nest("/api", api);
    // D2：ServeDir 指向 web/dist；缺 dist → `/` 显示构建指引页（防静默 404）。
    if state.web_root.join("index.html").exists() {
        router.fallback_service(ServeDir::new(status_root(state)))
    } else {
        router.fallback(get(build_guide))
    }
}

fn status_root(state: AppState) -> PathBuf {
    state.web_root
}

/// 缺 dist 时的构建指引（D2 条款，不静默 404）。
async fn build_guide() -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        "<!doctype html><meta charset=utf-8><title>live-audience</title>\
         <body style='font-family:sans-serif;background:#0f1117;color:#e8eaed;display:grid;\
         place-items:center;height:100vh;margin:0'>\
         <div><h2>前端尚未构建</h2><p>在本仓库 <code>web/</code> 下执行：\
         <pre style='background:#181b24;padding:12px 16px;border-radius:6px'>\
npm install\nnpm run build</pre>后重启 serve 即可。</div></body>",
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// /api/rooms
// ---------------------------------------------------------------------------

async fn rooms_list(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    // D3：现布局单房间——uid 暂恒等于 config 房号（路径形是设计承诺，非多房间实现）。
    Ok(Json(json!([{
        "id": config.bilibili.room_id,
        "project_name": config.project_name,
        "streamer_uid": config.bilibili.streamer_uid,
        "output_dir": config.output_dir.display().to_string(),
    }])))
}

// ---------------------------------------------------------------------------
// /api/config（D6 打码）
// ---------------------------------------------------------------------------

async fn config_get(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    let ai = &config.ai;
    let agent = &ai.agent;
    // D6：cookie/api_key 只回存在性布尔，永不回显原文。
    Ok(Json(json!({
        "project_name": config.project_name,
        "output_dir": config.output_dir.display().to_string(),
        "bilibili": {
            "room_id": config.bilibili.room_id,
            "streamer_uid": config.bilibili.streamer_uid,
            "cookie_present": !config.bilibili.cookie.trim().is_empty(),
            "additional_viewer_ids": config.bilibili.additional_viewer_ids,
        },
        "ai": {
            "api": ai.api,
            "base_url": ai.base_url,
            "model": ai.model,
            "api_key_present": !ai.api_key.trim().is_empty(),
            "reasoning": {
                "enabled": ai.reasoning.enabled,
                "effort": ai.reasoning.effort,
                "replay_content": ai.reasoning.replay_content,
            },
            "agent": {
                "max_turns": agent.max_turns,
                "run_retries": agent.run_retries,
                "retry_backoff_seconds": agent.retry_backoff_seconds,
                "local_trace": agent.local_trace,
            },
            "search_results_per_query": ai.search_results_per_query,
            "max_output_tokens": ai.max_output_tokens,
            "rules": ai.rules,
        },
        "writable_keys": WRITABLE_CONFIG_KEYS.iter().map(|(s, k)| format!("{s}.{k}")).collect::<Vec<_>>(),
    })))
}

/// D6：线路级 YAML 重写（保留注释与其余内容）——把 (段, 键) 定位段内首行
/// `  {key}:` 与整行替换为新值；找不到合法落点 → 422。
///
/// ag2-F3 加固三面：
/// - 多行值拒绝：行级重写只承载单行 scalar，换行会被 serde_yml 落成块标量撕裂布局；
/// - 重复键拒绝：同段内键出现 2 处以上 → 拒绝，不猜哪行是「真值」；
/// - 原子写：tmp 文件先过 load + validate 双门禁再 rename 落位——不合格配置
///   永远接触不到真路径，同时消除「半截写盘」窗口与「已落盘请检查」的脏姿势。
fn write_keys(
    config_path: &std::path::Path,
    patch: &[((&str, &str), String)],
) -> Result<(), String> {
    let text =
        std::fs::read_to_string(config_path).map_err(|error| format!("读取配置失败：{error}"))?;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let had_trailing_newline = text.ends_with('\n');
    for ((section, key), value) in patch {
        if value.contains(['\n', '\r']) {
            return Err(format!(
                "「{section}.{key}」不支持多行值（行级重写只承载单行 scalar）"
            ));
        }
        let header = format!("{section}:");
        let Some(section_idx) = lines.iter().position(|line| line.trim_end() == header) else {
            return Err(format!("配置缺少「{section}」段"));
        };
        let mut hits: Vec<usize> = Vec::new();
        for (index, line) in
            lines
                .iter()
                .enumerate()
                .skip(section_idx + 1)
                .take_while(|(_, line)| {
                    line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty()
                })
        {
            if line.trim_start().starts_with(&format!("{key}:")) {
                hits.push(index);
            }
        }
        match hits.as_slice() {
            [] => {
                return Err(format!(
                    "「{section}.{key}」在配置中找不到位置（不追加新键，请手写一行）"
                ));
            }
            [index] => {
                // 值含空格/井号/引号等 → serde_yml 走 quoted 形态，永不裸写。
                let frag = serde_yml::to_string(&Value::String(value.clone()))
                    .map_err(|error| format!("YAML 序列化失败：{error}"))?;
                let frag = frag.trim();
                let line = &mut lines[*index];
                let indent: String = line.chars().take_while(char::is_ascii_whitespace).collect();
                *line = format!("{indent}{key}: {frag}");
            }
            many => {
                return Err(format!(
                    "「{section}.{key}」在配置中出现 {} 处，拒绝猜测（请手工收敛为一行）",
                    many.len()
                ));
            }
        }
    }
    let mut out = lines.join("\n");
    if had_trailing_newline {
        out.push('\n');
    }
    // 原子序列：tmp → load+validate → rename。校验不过则 tmp 清走、原文件分毫未动。
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let tmp_path = config_path.with_file_name(format!(
        ".{file_name}.live-server-tmp-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, &out).map_err(|error| format!("写入临时配置失败：{error}"))?;
    let verdict = live_core::config::load_config(&tmp_path)
        .map_err(|error| error.to_string())
        .and_then(|config| {
            live_core::config::validate_for_collection(&config)
                .map_err(|error| error.to_string())
                .and(live_core::config::validate_for_ai(&config).map_err(|error| error.to_string()))
        });
    match verdict {
        Ok(()) => std::fs::rename(&tmp_path, config_path)
            .map_err(|error| format!("落位失败（原配置未动，见 {tmp_path:?}）：{error}")),
        Err(error) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(format!("写入后校验未通过，配置未改动：{error}"))
        }
    }
}

async fn config_put(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<Value>,
) -> AppResult<Json<Value>> {
    let object = match body.as_object() {
        Some(object) => object,
        None => {
            return Err(fail_box(
                StatusCode::UNPROCESSABLE_ENTITY,
                "配置写入必须是 JSON 对象",
            ));
        }
    };
    let mut patch: Vec<((&'static str, &'static str), String)> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();
    for (section, section_value) in object {
        let Some(section_map) = section_value.as_object() else {
            rejected.push(format!("{section} 必须是对象"));
            continue;
        };
        for (key, value) in section_map {
            let writable = WRITABLE_CONFIG_KEYS
                .iter()
                .find(|(s, k)| s == section && k == key);
            // D9 显式类型：非字符串值 → 422，不许被「空串=保持」沉默吞掉（ag2-F2）。
            if !value.is_string() {
                rejected.push(format!(
                    "{section}.{key} 的值必须是字符串（null→删除语义不支持）"
                ));
                continue;
            }
            let value_str = value.as_str().unwrap_or_default().to_string();
            match (writable, value_str.trim().is_empty()) {
                (_, true) => {} // 空串 = 保持现值（D6）
                (Some((s, k)), false) => {
                    if value_str.chars().count() > MAX_PUT_VALUE_CHARS {
                        return Err(fail_box(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            &format!("{s}.{k} 超出长度上限 {MAX_PUT_VALUE_CHARS}"),
                        ));
                    }
                    patch.push(((s, k), value_str));
                }
                (None, false) => rejected.push(format!("{section}.{key} 不在可写白名单")),
            }
        }
    }
    if !rejected.is_empty() {
        return Err(fail_box(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("拒绝的键：{}", rejected.join(", ")),
        ));
    }
    if patch.is_empty() {
        return Ok(Json(json!({"status": "unchanged"})));
    }
    // 校验已并入 write_keys 的 tmp 阶段（ag2-F3）：失败 → 422 且原文件分毫未动。
    if let Err(error) = write_keys(&state.config_path, &patch) {
        return Err(fail_box(StatusCode::UNPROCESSABLE_ENTITY, &error));
    }
    Ok(Json(json!({"status": "updated", "keys": patch.len()})))
}

// ---------------------------------------------------------------------------
// runs 面（B3）：POST 触发 + registry 轮询 + demo 静态快照
// ---------------------------------------------------------------------------

/// D3/D9：POST /api/runs {kind, force?, viewer_uid?} → 202 {run_id}。
///
/// 校验口径（kickoff B3b 冻结）：kind ∈ {full, viewer}；kind=viewer 必须给出
/// viewer_uid 且与 force 互斥（force 是全量清理语义，D7）；kind=full 与 viewer_uid
/// 互斥；非布尔 force / 超长 uid / 非对象体一律 422。
async fn runs_post(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<Value>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let object = body.as_object().ok_or_else(|| {
        fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "run 触发体必须是 JSON 对象",
        )
    })?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| crate::registry::RUN_KINDS.contains(kind))
        .ok_or_else(|| {
            fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "kind 必须是 full 或 viewer",
            )
        })?;
    let force = match object.get("force") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        _ => {
            return Err(fail(StatusCode::UNPROCESSABLE_ENTITY, "force 必须是布尔"));
        }
    };
    let viewer_uid = match object.get("viewer_uid") {
        None | Some(Value::Null) => None,
        Some(Value::String(uid)) if !uid.trim().is_empty() => {
            if uid.trim().chars().count() > MAX_VIEWER_UID_CHARS {
                return Err(fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &format!("viewer_uid 超出长度上限 {MAX_VIEWER_UID_CHARS}"),
                ));
            }
            Some(uid.trim().to_string())
        }
        _ => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "viewer_uid 必须是非空字符串",
            ));
        }
    };
    let viewer_uid = match (kind, viewer_uid) {
        ("viewer", None) => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "kind=viewer 必须给出 viewer_uid",
            ));
        }
        ("viewer", Some(_)) if force => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "kind=viewer 不接受 force（force 是全量清理语义）",
            ));
        }
        ("full", Some(_)) => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "kind=full 不接受 viewer_uid",
            ));
        }
        (_, viewer_uid) => viewer_uid,
    };
    let record = if state.demo {
        // G3 裁决：demo 模式返回已完成静态快照——合成、无网络、幂等（重复 POST
        // 返回同一 run_id；红线：合成不得伪装真实请求足迹）。
        state.registry.demo_snapshot(json!({
            "detail": "demo 模式：返回静态快照，不触发真实运行",
        }))
    } else {
        // ag3-F3：已有未终态 run → 409，客户端可从错文中拿到在飞 run_id 自行轮询。
        Registry::spawn_run(
            &state.registry,
            load_config(&state)?,
            kind,
            viewer_uid,
            force,
            state.bilibili_hosts.clone(),
        )
        .map_err(|active| {
            fail(
                StatusCode::CONFLICT,
                &format!("已有进行中的 run（{active}），待其到达终态后再触发"),
            )
        })?
    };
    let run_id = record.lock().expect("record poisoned").run_id.clone();
    Ok((StatusCode::ACCEPTED, Json(json!({"run_id": run_id}))))
}

async fn run_get(State(state): State<AppState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    match state.registry.get(&id) {
        Some(record) => Ok(Json(crate::registry::run_to_json(
            &record.lock().expect("record poisoned"),
        ))),
        None => Err(fail_box(StatusCode::NOT_FOUND, &format!("run {id} 不存在"))),
    }
}

// ---------------------------------------------------------------------------
// 房间数据面（B2）：overview / viewers / tree / graph
// ---------------------------------------------------------------------------

/// D9：路径参数中观众 vid 的消毒限值（长度顶与 POST 同一口径：64 宽限值上限）。
pub const MAX_VID_PATH_CHARS: usize = 64;
/// D9：vid 合法字符集（alnum + "_" + "-"）。B 站 uid 是数字串，demo uid 走
/// 「demo-N」形——制表与连字符之外的一律视作穿透恶意（%2F 经 axum 解码后落此）。
fn vid_guard(vid: &str) -> AppResult<()> {
    let legal = !vid.is_empty()
        && vid.len() <= MAX_VID_PATH_CHARS
        && vid
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if !legal {
        return Err(fail(StatusCode::NOT_FOUND, &format!("观众 {vid} 不存在")));
    }
    Ok(())
}

/// uid 守卫：D3 路径形承诺——现布局单房间，uid 暂恒等于 config 房号，其他值一律 404。
fn room_guard(config: &live_core::config::Config, uid: &str) -> AppResult<()> {
    if config.bilibili.room_id != uid {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("room {uid} 不存在（单房间布局）"),
        ));
    }
    Ok(())
}

fn read_json(path: &std::path::Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// graph 文件存在才开库（Store::open 会建文件，纯读路径禁止写入副作用）。
fn open_graph(root: &std::path::Path) -> Option<live_core::graph::store::Store> {
    let path = root.join("graph").join("perception.sqlite3");
    path.exists()
        .then(|| live_core::graph::store::Store::open(&path).ok())
        .flatten()
}

/// 无图态的 delta 形状（与 live_core::graph::query::run_pair_delta 的 baseline 臂同形）。
const BASELINE_DELTA: &str = r#"{"baseline_only":true,"from_run_id":null,"to_run_id":null,"interest":{"opened":[],"closed":[],"changed":[]},"guards":{"added":[],"removed":[]}}"#;

async fn room_overview(
    State(state): State<AppState>,
    Path(uid): Path<String>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    let root = data_root(&state)?;
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
    let rows = live_core::leads::read_ledger(&live_core::leads::ledger_path(&root));
    let count =
        |status: live_core::leads::LeadStatus| rows.iter().filter(|r| r.status == status).count();
    let delta = match open_graph(&root) {
        Some(store) => live_core::graph::query::run_pair_delta(&store)
            .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?,
        None => serde_json::from_str(BASELINE_DELTA).expect("literal parses"),
    };
    Ok(Json(json!({
        "room_id": config.bilibili.room_id,
        "streamer_uid": config.bilibili.streamer_uid,
        "project_name": config.project_name,
        "collection": collection,
        "ai": read_json(&root.join("ai").join("state.json")),
        "situation": read_json(&root.join("ai").join("situation.json")),
        "leads": {
            "summary": live_core::leads::summary_line(&rows, None),
            "totals": {
                "pending_approval": count(live_core::leads::LeadStatus::PendingApproval),
                "approved": count(live_core::leads::LeadStatus::Approved),
                "consumed": count(live_core::leads::LeadStatus::Consumed),
                "rejected": count(live_core::leads::LeadStatus::Rejected),
                "deferred": count(live_core::leads::LeadStatus::Deferred),
            },
            // 人工审批面：pending 明细直出（前端列表渲染；响度按 viewer 分组）
            "pending": rows.iter()
                .filter(|r| r.status == live_core::leads::LeadStatus::PendingApproval)
                .collect::<Vec<_>>(),
        },
        "delta": delta,
    })))
}

async fn room_viewers(
    State(state): State<AppState>,
    Path(uid): Path<String>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    let root = data_root(&state)?;
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
        viewers.push(json!({
            "uid": uid,
            "name": viewer["viewer"]["name"].as_str()
                .or_else(|| viewer["profile"]["name"].as_str()),
            "collected_at": viewer["collected_at"],
            "ai_status": cached.as_ref().map(|c| c["status"].clone()),
            // 空池引导位约定：front-end 按 completed=false + viewer 数=0 渲染引导。
            "ai_completed": cached.as_ref().is_some_and(|c| c["status"] == "complete"),
        }));
    }
    Ok(Json(json!(viewers)))
}

/// project 用于一切图端点：interest_states 是面板信息主源。
fn project_for_viewer(
    root: &std::path::Path,
) -> AppResult<(live_core::graph::store::Store, Value)> {
    let store = open_graph(root).ok_or_else(|| {
        fail(
            StatusCode::NOT_FOUND,
            "图尚未落盘——先跑过 Audience 阶段再取",
        )
    })?;
    let value = live_core::graph::project::project(
        &store,
        &live_core::graph::project::ProjectOptions {
            include_episodes: false,
            include_interest_states: true,
            include_situation_actions: false,
            // 面板展示取全史：闸门左半（FIND-5）恒真。
            current_run_id: None,
            ..live_core::graph::project::ProjectOptions::default()
        },
    )
    .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    Ok((store, value))
}

async fn viewer_graph(
    State(state): State<AppState>,
    Path((uid, vid)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    vid_guard(&vid)?;
    let root = data_root(&state)?;
    let (_store, value) = project_for_viewer(&root)?;
    Ok(Json(crate::cytoscape::scoped(
        &value,
        &format!("viewer:{vid}"),
    )))
}

async fn room_graph(
    State(state): State<AppState>,
    Path(uid): Path<String>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    let root = data_root(&state)?;
    let (_store, value) = project_for_viewer(&root)?;
    Ok(Json(crate::cytoscape::elements(&value)))
}

async fn viewer_tree(
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
            live_core::graph::query::episodes(&store, &vid, None)
                .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?,
            live_core::graph::query::mentions_of_viewer(&store, &vid, None)
                .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?,
        ),
        // 图尚未落盘 → 信息空面（viewer 原料 + ai 缓存仍可读，写盘前态）。
        None => (Vec::new(), Vec::new()),
    };
    Ok(Json(json!({
        "uid": vid,
        "viewer": viewer,
        "ai": read_json(
            &root
                .join("ai")
                .join("perception")
                .join("viewers")
                .join(format!("{vid}.json"))
        ),
        "episodes": episodes,
        "mentions": mentions,
    })))
}

// ---------------------------------------------------------------------------
// server 启动（serve/run 共用面）
// ---------------------------------------------------------------------------

pub struct StartOptions {
    pub config_path: PathBuf,
    pub port: u16,
    pub web_root: PathBuf,
    pub demo: bool,
    /// demo（_demo 根）态数据呈现覆盖（D5）。
    pub data_root: Option<PathBuf>,
    /// 测试接缝：POST runs → Bilibili 根地址注入（生产 None；见 AppState 同名字段）。
    pub bilibili_hosts: Option<(String, String)>,
}

/// 服务位于 127.0.0.1（M5 范围：不做鉴权/多用户——绑定地址即条款）。
pub fn serve(options: StartOptions) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("tokio runtime: {error}"))?;
    runtime.block_on(async {
        let state = AppState {
            config_path: options.config_path,
            web_root: options.web_root,
            registry: Registry::new(),
            demo: options.demo,
            data_root: options.data_root,
            bilibili_hosts: options.bilibili_hosts,
        };
        let addr = SocketAddr::from(([127, 0, 0, 1], options.port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|error| format!("绑定 {addr} 失败：{error}"))?;
        eprintln!(
            "live-audience serve 已启动：http://127.0.0.1:{}",
            options.port
        );
        axum::serve(listener, build_app(state))
            .await
            .map_err(|error| format!("axum failure: {error}"))
    })
}
