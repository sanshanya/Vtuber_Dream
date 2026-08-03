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

fn fail_box(status: StatusCode, message: &str) -> AppFail {
    AppFail::new(status, message)
}

pub fn build_app(state: AppState) -> Router {
    let api = Router::new()
        .route("/rooms", get(rooms_list))
        .route("/rooms/:uid/overview", get(room_overview))
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
fn write_keys(
    config_path: &std::path::Path,
    patch: &[((&str, &str), String)],
) -> Result<(), String> {
    let text =
        std::fs::read_to_string(config_path).map_err(|error| format!("读取配置失败：{error}"))?;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let had_trailing_newline = text.ends_with('\n');
    for ((section, key), value) in patch {
        let header = format!("{section}:");
        let Some(section_idx) = lines.iter().position(|line| line.trim_end() == header) else {
            return Err(format!("配置缺少「{section}」段"));
        };
        let mut replaced = false;
        for line in lines.iter_mut().skip(section_idx + 1).take_while(|line| {
            line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty()
        }) {
            if line.trim_start().starts_with(&format!("{key}:")) {
                // 值含空格/井号/引号等 → serde_yml 走 quoted 形态，永不裸写。
                let frag = serde_yml::to_string(&Value::String(value.clone()))
                    .map_err(|error| format!("YAML 序列化失败：{error}"))?;
                let frag = frag.trim();
                let indent: String = line.chars().take_while(char::is_ascii_whitespace).collect();
                *line = format!("{indent}{key}: {frag}");
                replaced = true;
                break;
            }
        }
        if !replaced {
            return Err(format!(
                "「{section}.{key}」在配置中找不到位置（不追加新键，请手写一行）"
            ));
        }
    }
    let mut out = lines.join("\n");
    if had_trailing_newline {
        out.push('\n');
    }
    std::fs::write(config_path, out).map_err(|error| format!("写入配置失败：{error}"))
}

async fn config_put(
    State(state): State<AppState>,
    Json(body): Json<Value>,
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
    if let Err(error) = write_keys(&state.config_path, &patch) {
        return Err(fail_box(StatusCode::UNPROCESSABLE_ENTITY, &error));
    }
    // D6：写后走 config 同款校验（数值层未动，security 键加载 + ai 校验通过即视为一致）。
    match live_core::config::load_config(&state.config_path)
        .map_err(|error| error.to_string())
        .and_then(|config| {
            live_core::config::validate_for_collection(&config)
                .map_err(|error| error.to_string())
                .and(live_core::config::validate_for_ai(&config).map_err(|error| error.to_string()))
        }) {
        Ok(()) => Ok(Json(json!({"status": "updated", "keys": patch.len()}))),
        Err(error) => Err(fail_box(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("写入后校验失败（已落盘，请检查）：{error}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// runs 面（B3 补剧本；B1 仅打点位：demo 快照 + 不存在）
// ---------------------------------------------------------------------------

async fn runs_post() -> AppFail {
    // B3 补 spawn 通道；B1：demo 模式之外一律报名。
    fail(StatusCode::NOT_IMPLEMENTED, "run 触发通道在 M5-B3 接线")
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
// overview（B2 补；B1 打点位）
// ---------------------------------------------------------------------------

async fn room_overview(
    State(state): State<AppState>,
    Path(_uid): Path<String>,
) -> AppResult<Json<Value>> {
    load_config(&state)?;
    Err(fail_box(
        StatusCode::NOT_IMPLEMENTED,
        "overview 在 M5-B2 接线",
    ))
}

// ---------------------------------------------------------------------------
// server 启动（serve/run 共用面）
// ---------------------------------------------------------------------------

pub struct StartOptions {
    pub config_path: PathBuf,
    pub port: u16,
    pub web_root: PathBuf,
    pub demo: bool,
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
