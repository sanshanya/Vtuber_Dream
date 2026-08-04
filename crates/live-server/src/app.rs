//! axum 装配与端点（D1/D3/D9 + B1 切：rooms + config + 静态面）。
//!
//! 与安全面相干的所有魔数就地命名（kickoff 完成定义 5）。
//!
//! 体积备书（r8-F2/ag8-F3，G1 维护缝后约 1040 行已破 800 线）：端点表面是单房间
//! 小 API（十二条路由），共享同一套 DTO/错误形态/闸限；拆出 handler 子卷会把
//! 「端点表 ↔ 路由表」一眼对照打散。出现第二房间形态（多房间真实依赖）时按
//! rooms/config/runs 三卷拆分——maintenance 两路由是同房间形态，不触发拆分。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
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
    /// W1/r2-F3：config 写互斥——并发 PUT 的 read-modify-write 会互相覆盖丢更新。
    /// write_keys 是同步原子写（tmp+rename），持锁窗口不含 await，std Mutex 足够。
    pub config_write_lock: Arc<Mutex<()>>,
    /// Z6/P0-6：graph artifact 重建互斥——并发首访同时 miss 时只许一个线程重建
    /// （≈0.6s SQL + 压缩），其余等待者复用其产物。持锁在 spawn_blocking 内。
    pub graph_artifact_lock: Arc<Mutex<()>>,
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
        .route("/rooms/:uid/leads/:lead_id/approve", post(lead_approve))
        .route(
            "/rooms/:uid/maintenance/entity_split",
            post(maintenance_entity_split),
        )
        .route(
            "/rooms/:uid/maintenance/entity_merge",
            post(maintenance_entity_merge),
        )
        .route("/config", get(config_get).put(config_put))
        .route("/runs", axum::routing::post(runs_post))
        .route("/runs/:id", get(run_get))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state.clone());

    let router = Router::new().nest("/api", api);
    // D2：ServeDir 指向 web/dist；缺 dist → `/` 显示构建指引页（防静默 404）。
    // Z4/P0-7：消费 vite 预压缩产物——按 Accept-Encoding 协商 .br/.gz，无则回落原文件。
    if state.web_root.join("index.html").exists() {
        router.fallback_service(
            ServeDir::new(status_root(state))
                .precompressed_gzip()
                .precompressed_br(),
        )
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
npm ci\nnpm run build</pre>后重启 serve 即可。</div></body>",
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
        Ok(()) => match std::fs::rename(&tmp_path, config_path) {
            Ok(()) => Ok(()),
            Err(error) => {
                // W1/r2-F2：rename 失败同样清走 tmp；错文不透服务器绝对路径。
                let _ = std::fs::remove_file(&tmp_path);
                Err(format!("落位失败（原配置未动）：{error}"))
            }
        },
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
    // W1/r2-F3：read-modify-write 全程持锁，并发 PUT 不得相互覆盖。write_keys 是同步块，
    // 持锁窗口不跨 await；锁中毒 = 上一次持锁者已 panic，用 expect 露头而非封锁修复。
    let _write_guard = state
        .config_write_lock
        .lock()
        .expect("config write lock poisoned");
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
        .filter(|kind| {
            crate::registry::RUN_KINDS.contains(kind)
                || crate::registry::RUN_KINDS_STAGED.contains(kind)
        })
        .ok_or_else(|| {
            fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "kind 必须是 full / viewer / collect_streamer / collect_guards / ai_viewers / ai_audience",
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
            let trimmed = uid.trim();
            // r2-F1：写侧穿透——collector 容错链以 input uid 落盘 viewers/{uid}.json，
            // 字符集守卫必须与 vid_guard 同集（`, %2F → /` 皆在此拒）。
            if !uid_charset_legal(trimmed, MAX_VIEWER_UID_CHARS) {
                return Err(fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &format!("viewer_uid 非法：限 1..={MAX_VIEWER_UID_CHARS} 位 [A-Za-z0-9_-]"),
                ));
            }
            Some(trimmed.to_string())
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
        // Z4：分层四 kind 是纯整体动作——不接 viewer_uid（单人=kind=viewer）。
        (kind, Some(_)) if crate::registry::RUN_KINDS_STAGED.contains(&kind) => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("kind={kind} 不接受 viewer_uid（单舰长动作 = kind=viewer）"),
            ));
        }
        // Z4：分层四 kind 亦不接 force——force=全量清 AI 缓存语义仅属 kind=full；
        // ai_* 的幂等就是保留在Collect面的前提下做哈希失配重算。
        (kind, _) if crate::registry::RUN_KINDS_STAGED.contains(&kind) && force => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("kind={kind} 不接受 force（force 仅属 kind=full 的全量语义）"),
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
/// D9/W1：vid/viewer_uid 共用合法字符集——alnum + "_" + "-"。B 站 uid 是数字串，
/// demo uid 走「demo-N」形——制表与连字符之外一律视作穿透恶意（%2F 经 axum
/// 解码后落此；写侧 pane 同集守卫是 r2-F1 的纵深一对）。
pub fn uid_charset_legal(id: &str, max_len: usize) -> bool {
    !id.is_empty()
        && id.len() <= max_len
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn vid_guard(vid: &str) -> AppResult<()> {
    if !uid_charset_legal(vid, MAX_VID_PATH_CHARS) {
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
    let store = open_graph(&root);
    // G2 表形态（design §9.2 行 254）：leads 读面唯一源 = discovery_leads 表。
    // 库在而旧 JSONL 仍在 → 此处一次性入库归档（幂等迁移的读面触点；守卫失败
    // 即 500 响铃，绝不带病半账出面）。无库 → 空集合（M4.x 无账本的同义面）。
    let rows = match &store {
        Some(store) => {
            live_core::leads::migrate_jsonl(store, &root)
                .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
            live_core::leads::read_rows(store)
                .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?
        }
        None => Vec::new(),
    };
    let count =
        |status: live_core::leads::LeadStatus| rows.iter().filter(|r| r.status == status).count();
    let delta = match &store {
        Some(store) => live_core::graph::query::run_pair_delta(store)
            .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?,
        None => serde_json::from_str(BASELINE_DELTA).expect("literal parses"),
    };
    // Z3 首页指标条：无图态 → null 空态（前端呈现「—」而非臆造数字）。
    let graph_stats = match &store {
        Some(store) => Some(
            live_core::graph::query::graph_stats(store)
                .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?,
        ),
        None => None,
    };
    // Z5/C1：BriefingCard refs 可点的归属解析面——episode_id → {viewer_id, title}。
    // 轻量投影（GRAPH_QUERY_LIMIT=500 帽，超出部分 ref 落未解析态、chip 不可点），
    // 不抄整行（fields/platform_facts 大键留在 tree/graph 端点）。
    let episode_index: Value = match &store {
        Some(store) => {
            let rows = live_core::graph::query::episodes(store, "", None)
                .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
            let map = rows
                .iter()
                .filter_map(|row| {
                    let episode_id = row["episode_id"].as_str()?;
                    let viewer_id = row["viewer_id"].as_str()?;
                    Some((
                        episode_id.to_string(),
                        json!({"viewer_id": viewer_id, "title": row["title"]}),
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
        // 主播卡数据面（Z2 主页签名）：streamer.json 的 profile 段原样透传——
        // sources.videos 属事实层原料且体大，不上 overview 面；缺文件 → null 空态。
        "streamer": read_json(&root.join("streamer.json"))
            .and_then(|v| v.get("profile").cloned()),
        // 直播数据页档案面：shared/live_records.json 整场记录原样透传
        //（status/count/records[]；空态 status="empty" 由前端解说）。
        "live": read_json(&root.join("shared").join("live_records.json")),
        // Z3：图存量指标面（旧版报告顶部数字条）。
        "graph_stats": graph_stats,
        // Z5/C1：BriefingCard ref → 归属观众树页的解析索引（无图态 → {} 空态）。
        "episode_index": episode_index,
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
            // G2-B：自治位读取面（Leads 页标题行 L1 状态徽标）
            "autonomy": config.collection.leads_autonomy,
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
            // Z3：大航海 API 一发即带的身份面（旧版站的舰长签名：头像+舰长等级+勋章）——
            // face 是 hdslb 图床 URL，呈现侧须 referrerPolicy="no-referrer"。
            "face": viewer["viewer"]["face"].as_str()
                .or_else(|| viewer["profile"]["face"].as_str()),
            "guard_level": viewer["viewer"]["guard_level"].as_i64(),
            "medal_level": viewer["viewer"]["medal_level"].as_i64(),
            "collected_at": viewer["collected_at"],
            "ai_status": cached.as_ref().map(|c| c["status"].clone()),
            // 空池引导位约定：front-end 按 completed=false + viewer 数=0 渲染引导。
            "ai_completed": cached.as_ref().is_some_and(|c| c["status"] == "complete"),
            // Z5c 时效位：旧 AI 结论保留但信源已变 → 行面亮「信源已更新·待重判」。
            // null（无参考旧结论 / 非 complete）与 false（绿灯时效内）区分。
            "ai_stale": cached
                .as_ref()
                .and_then(|c| live_core::agent::pipeline::viewer_perception_stale(&config, &viewer, c)),
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

// ---------------------------------------------------------------------------
// Z6/P0-6：整体图谱端点——默认折叠视图走外置物化（trio + ETag/304）；
// `?kinds=all` 全量逃生门与 `?kinds=A,B` 自定义折叠走现算直通。
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct RoomGraphParams {
    kinds: Option<String>,
}

/// kinds 查询参数 → 折叠集：None = 配置白名单；"all" = 全谱直通（无缓存）；
/// csv = 自定义折叠（现算，无缓存）。未知类 → 响亮 400（配置面治错原则同穿透到查询面）。
fn resolve_graph_kinds(
    raw: Option<&str>,
    default_whitelist: &[String],
) -> AppResult<Option<std::collections::BTreeSet<String>>> {
    let Some(text) = raw else {
        return Ok(Some(default_whitelist.iter().cloned().collect()));
    };
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let mut set = std::collections::BTreeSet::new();
    for part in trimmed.split(',') {
        let kind = part.trim();
        if kind.is_empty() {
            continue;
        }
        if !live_core::config::GRAPH_KIND_ALLOWLIST.contains(&kind) {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                &format!(
                    "kinds 未知节点类 \"{kind}\"（允许：{}；或 all = 全谱）",
                    live_core::config::GRAPH_KIND_ALLOWLIST.join("/")
                ),
            ));
        }
        set.insert(kind.to_string());
    }
    if set.is_empty() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "kinds 不可解析为空集（允许：Viewer/Entity/Episode/Mention/InterestState/Situation/Action；或 all = 全谱）",
        ));
    }
    Ok(Some(set))
}

/// 现算直通（all / 自定义 kinds）：project + fold 一次性产出 JSON 字节。
/// project() 是同步 rusqlite（s0 全量 ≈0.6s），放 spawn_blocking 避免卡 executor。
async fn compute_graph_bytes(
    root: PathBuf,
    kinds: Option<std::collections::BTreeSet<String>>,
) -> AppResult<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let (_store, value) = project_for_viewer(&root)?;
        let json = match &kinds {
            None => crate::cytoscape::elements(&value),
            Some(expanded) => crate::cytoscape::elements_expanded(&value, expanded),
        };
        Ok::<_, AppFail>(json.to_string().into_bytes())
    })
    .await
    .map_err(|join| fail(StatusCode::INTERNAL_SERVER_ERROR, &join.to_string()))?
}

/// 物化确保（内容寻址）：探针 etag → 档命中直返；缺/陈旧 → 持锁双查重建。
/// 全部 IO/SQL 收进 spawn_blocking（rusqlite + trio 文件读都是同步阻塞面）。
async fn ensure_graph_artifact(
    root: PathBuf,
    kinds: Vec<String>,
    lock: Arc<Mutex<()>>,
) -> AppResult<crate::graph_artifact::GraphArtifact> {
    if !root.join("graph").join("perception.sqlite3").exists() {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "图尚未落盘——先跑过 Audience 阶段再取",
        ));
    }
    let kinds_csv = kinds.join(",");
    tokio::task::spawn_blocking(move || {
        // 探针（毫秒级行扫）= 失效指纹 + 内容寻址 ETag（misfire 选型见 graph_artifact 卷首注）。
        let store = open_graph(&root).ok_or_else(|| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "graph 库存在但不可开（Store::open 失败）",
            )
        })?;
        let etag = crate::graph_artifact::content_probe(&store, &kinds_csv)
            .map_err(|err| fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()))?;
        if let Some(artifact) = crate::graph_artifact::read_artifact(&root, &etag) {
            return Ok(artifact);
        }
        let _guard = lock
            .lock()
            .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "graph artifact 锁中毒"))?;
        // 双查：等待锁期间另一线程可能已重建（重探 = 同店 scan，零成本）。
        let etag2 = crate::graph_artifact::content_probe(&store, &kinds_csv)
            .map_err(|err| fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()))?;
        if let Some(artifact) = crate::graph_artifact::read_artifact(&root, &etag2) {
            return Ok(artifact);
        }
        let expanded: std::collections::BTreeSet<String> = kinds.iter().cloned().collect();
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
        .map_err(|err| fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()))?;
        drop(store);
        let folded = crate::cytoscape::elements_expanded(&value, &expanded);
        let artifact =
            crate::graph_artifact::write_artifact(&root, &etag2, folded.to_string().as_str())
                .map_err(|err| fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()))?;
        Ok(artifact)
    })
    .await
    .map_err(|join| fail(StatusCode::INTERNAL_SERVER_ERROR, &join.to_string()))?
}

/// If-None-Match 宽松比对（多值/弱校验前缀均容忍——读面协商，非安全面）。
fn if_none_match_hit(headers: &HeaderMap, etag: &str) -> bool {
    let Some(raw) = headers.get(axum::http::header::IF_NONE_MATCH) else {
        return false;
    };
    let Ok(text) = raw.to_str() else { return false };
    let quoted = format!("\"{etag}\"");
    text.split(',').any(|candidate| {
        let t = candidate.trim().trim_start_matches("W/");
        t == quoted || t == etag || t == "*"
    })
}

fn graph_body_response(etag: &str, encoding: Option<&str>, bytes: Vec<u8>) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )
        .header(axum::http::header::ETAG, format!("\"{etag}\""))
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::VARY, "Accept-Encoding");
    if let Some(encoding) = encoding {
        builder = builder.header(axum::http::header::CONTENT_ENCODING, encoding);
    }
    builder
        .body(axum::body::Body::from(bytes))
        .expect("静态头组装不会失败")
}

async fn room_graph(
    State(state): State<AppState>,
    Path(uid): Path<String>,
    headers: HeaderMap,
    Query(params): Query<RoomGraphParams>,
) -> AppResult<Response> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    let root = data_root(&state)?;
    let kinds = resolve_graph_kinds(
        params.kinds.as_deref(),
        &config.perception.graph_default_expanded_kinds,
    )?;
    // 全谱直通（?kinds=all）：保持 Z6 前的原始面（全量 Json，无物化）。
    let Some(expanded) = kinds else {
        let bytes = compute_graph_bytes(root, None).await?;
        return Ok(graph_body_response("unversioned-all", None, bytes));
    };
    let default_set: std::collections::BTreeSet<String> = config
        .perception
        .graph_default_expanded_kinds
        .iter()
        .cloned()
        .collect();
    // 自定义 csv（≠配置白名单）：现算直通，不养第二套缓存键。
    if expanded != default_set {
        let bytes = compute_graph_bytes(root, Some(expanded)).await?;
        return Ok(graph_body_response("unversioned-custom", None, bytes));
    }
    // 默认视图：物化协商通道（ETag 304 + 预压缩 trio）。
    let artifact = ensure_graph_artifact(
        root,
        config.perception.graph_default_expanded_kinds.clone(),
        state.graph_artifact_lock.clone(),
    )
    .await?;
    if if_none_match_hit(&headers, &artifact.etag) {
        return Ok(Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(axum::http::header::ETAG, format!("\"{}\"", artifact.etag))
            .header(axum::http::header::CACHE_CONTROL, "no-cache")
            .body(axum::body::Body::empty())
            .expect("304 静态组装不会失败"));
    }
    // Accept-Encoding 协商：br 优先，gzip 次之，裸 JSON 兜底。q 值忽略（内部工具面）。
    let accept = headers
        .get(axum::http::header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (encoding, bytes) = if accept.contains("br") && artifact.br.is_some() {
        (Some("br"), artifact.br.expect("just checked"))
    } else if accept.contains("gzip") && artifact.gz.is_some() {
        (Some("gzip"), artifact.gz.expect("just checked"))
    } else {
        (None, artifact.raw)
    };
    Ok(graph_body_response(&artifact.etag, encoding, bytes))
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
/// - 不存在（lead_id 未知 / 房间错 / 穿透形 id）→ 404（D3 错误形态）；
/// - 非法迁移（consumed/rejected/deferred 源态）→ 422，错文讲规则 + 当前状态；
/// - MXA-1 延伸：账本迁移守卫失败（旧 JSONL 含坏行）→ 500 响铃，绝不带病写。
async fn lead_approve(
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
    let store = live_core::graph::store::Store::open(&store_path)
        .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    live_core::leads::migrate_jsonl(&store, &root)
        .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    let Some(mut row) = store
        .lead_row(&lead_id)
        .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?
    else {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("lead {lead_id} 不存在"),
        ));
    };
    let changed = live_core::leads::approve_transition(row.status)
        .map_err(|message| fail(StatusCode::UNPROCESSABLE_ENTITY, &message))?;
    if changed {
        row.status = live_core::leads::LeadStatus::Approved;
        store
            .update_lead_row(&row)
            .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    }
    Ok(Json(json!({
        "dedupe_key": row.dedupe_key,
        "status": live_core::leads::status_name(row.status),
        // 为幂等重放留可观测位：终态（dedupe_key/status）恒定，仅动作位分野。
        "changed": changed,
    })))
}

// ---------------------------------------------------------------------------
// 图维护缝（G1 / design §8.6 行 224-229）：
// POST /api/rooms/:uid/maintenance/entity_split|entity_merge
// ---------------------------------------------------------------------------

use live_core::graph::store::{MaintenanceError, Store as GraphStore};

/// 维护缝的开库点：无图 = 404（写路径不得靠 Store::open 顺手建空库）。
fn maintenance_store(root: &std::path::Path) -> AppResult<GraphStore> {
    open_graph(root).ok_or_else(|| {
        fail(
            StatusCode::NOT_FOUND,
            "图尚未落盘——先跑过 Audience 阶段再维护",
        )
    })
}

/// MaintenanceError → D3 错误形态：未知实体/mention=404，参数/归属语义=422，落库=500。
fn maintenance_fail(error: MaintenanceError) -> AppFail {
    match error {
        MaintenanceError::NotFound(message) => fail(StatusCode::NOT_FOUND, &message),
        MaintenanceError::Invalid(message) => fail(StatusCode::UNPROCESSABLE_ENTITY, &message),
        MaintenanceError::Store(error) => {
            fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
    }
}

/// 必带非空字符串成员（缺键/空串/非字符串 → 422）。
fn required_str<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> AppResult<&'a str> {
    match object.get(key).and_then(Value::as_str).map(str::trim) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("{key} 必须是非空字符串"),
        )),
    }
}

/// 必带非空字符串数组（缺键/非数组/空数组/成员不是非空字符串 → 422）。
fn required_str_list(object: &serde_json::Map<String, Value>, key: &str) -> AppResult<Vec<String>> {
    let Some(array) = object.get(key).and_then(Value::as_array) else {
        return Err(fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("{key} 必须是非空字符串数组"),
        ));
    };
    if array.is_empty() {
        return Err(fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("{key} 不能为空数组"),
        ));
    }
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        match item.as_str().map(str::trim) {
            Some(value) if !value.is_empty() => out.push(value.to_string()),
            _ => {
                return Err(fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &format!("{key} 的每一项必须是非空字符串"),
                ));
            }
        }
    }
    Ok(out)
}

/// 响应幂等键的回显与 store 的 canon 同齿（排序去重；审计与重放以集合为身份）。
fn echo_canon(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids.dedup();
    ids
}

/// run_id 空串 = 重放且原账不可考（理论面：detail 扫描脱靶）——响应对外为 null。
fn run_id_json(run_id: &str) -> Value {
    if run_id.is_empty() {
        Value::Null
    } else {
        Value::String(run_id.to_string())
    }
}

/// `POST /api/rooms/:uid/maintenance/entity_split`
/// 体：`{"entity_id": "...", "mention_ids": ["...", ...]}`。
/// 幂等（§8.6 行 229）：同参重放 = 同终态（changed=false、run_id 指回原始维护
/// run、不增生账）；不属该实体的 mention 显式 422，未知实体/mention 404。
async fn maintenance_entity_split(
    State(state): State<AppState>,
    Path(uid): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    let store = maintenance_store(&data_root(&state)?)?;
    let object = body.as_object().ok_or_else(|| {
        fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "维护操作体必须是 JSON 对象",
        )
    })?;
    let entity_id = required_str(object, "entity_id")?;
    let mention_ids = required_str_list(object, "mention_ids")?;
    let outcome = store
        .entity_split(entity_id, &mention_ids)
        .map_err(maintenance_fail)?;
    Ok(Json(json!({
        "op": "entity_split",
        "entity_id": entity_id,
        "mention_ids": echo_canon(mention_ids),
        "new_entity_id": outcome.new_entity_id,
        "moved_mentions": outcome.moved_mentions,
        "closed_edges": outcome.closed_edges,
        "changed": outcome.changed,
        "run_id": run_id_json(&outcome.run_id),
    })))
}

/// `POST /api/rooms/:uid/maintenance/entity_merge`
/// 体：`{"source_ids": ["..."], "target_id": "..."}`。幂等同 split 缝。
async fn maintenance_entity_merge(
    State(state): State<AppState>,
    Path(uid): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    let store = maintenance_store(&data_root(&state)?)?;
    let object = body.as_object().ok_or_else(|| {
        fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "维护操作体必须是 JSON 对象",
        )
    })?;
    let source_ids = required_str_list(object, "source_ids")?;
    let target_id = required_str(object, "target_id")?;
    let outcome = store
        .entity_merge(&source_ids, target_id)
        .map_err(maintenance_fail)?;
    Ok(Json(json!({
        "op": "entity_merge",
        "source_ids": echo_canon(source_ids),
        "target_id": target_id,
        "repointed_edges": outcome.repointed_edges,
        "folded_edges": outcome.folded_edges,
        "merged_aliases": outcome.merged_aliases,
        "changed": outcome.changed,
        "run_id": run_id_json(&outcome.run_id),
    })))
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
    let cached = read_json(
        &root
            .join("ai")
            .join("perception")
            .join("viewers")
            .join(format!("{vid}.json")),
    );
    // Z5c 时效位：与 room_viewers 行完全同源——cached 存在且 complete 才判哈希；
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
            config_write_lock: Default::default(),
            graph_artifact_lock: Default::default(),
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
