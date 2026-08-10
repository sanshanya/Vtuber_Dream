//! axum 装配与端点（装配面切 B1：rooms + config + 静态面）。
//!
//! 与安全面相干的所有魔数就地命名（kickoff 完成定义 5）。
//!
//! 体积备书已兑现（拆分条款触发）：头注自书「出现第二房间形态
//! （多房间真实依赖）时按 rooms/config/runs 拆分」——`/rooms/:uid/*` 全家桶路由
//! 已落地，本卷拆为五子卷 + 根本卷：根本卷 = 路由表/装配/共享面（状态/错误形态/
//! 守卫/开库），子卷 = rooms（房间数据面 + leads 审批）、graph_routes（图端点 +
//! 物化协商）、config_routes（白名单写面）、runs（触发/轮询）、maintenance（图维护）。
//! 公共常量经根 re-export（`pub use`），`app::X` 外部路径零变化。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde_json::json;
use tower_http::services::ServeDir;

use crate::registry::Registry;

mod config_routes;
mod graph_routes;
mod maintenance;
mod rooms;
mod runs;

pub use runs::MAX_VIEWER_UID_CHARS;

/// serve/run 共用默认端口（魔数命名）。
pub const DEFAULT_PORT: u16 = 3781;
/// 任何 JSON 请求体的上限（POST runs / PUT config 共用口径）。
pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
/// 路径参数中观众 vid 的消毒限值（长度顶与 POST 同一口径：64 宽限值上限）。
pub const MAX_VID_PATH_CHARS: usize = 64;

#[derive(Clone)]
pub struct AppState {
    pub config_path: PathBuf,
    /// 静态面根（不内嵌；生产 cwd/web/dist、测试可注入）。
    pub web_root: PathBuf,
    pub registry: Registry,
    /// 测试接缝：POST runs → spawn 的 Bilibili 根地址注入（生产 None → 官方端点）。
    pub bilibili_hosts: Option<(String, String)>,
    /// graph artifact 重建互斥——并发首访同时 miss 时只许一个线程重建
    /// （≈0.6s SQL + 压缩），其余等待者复用其产物。持锁在 spawn_blocking 内。
    pub graph_artifact_lock: Arc<Mutex<()>>,
}

/// 统一错误包装记类型：状态码 + {"error": 文案}（统一错误形态）。
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

pub(super) fn fail(status: StatusCode, message: &str) -> AppFail {
    AppFail::new(status, message)
}

/// 500 族同义闸（样板收口，B5 先例：≥5 处同款样板入公共件）——
/// app 五子卷共 21 处 `.map_err(|e| fail(500, &e.to_string()))` 同款。
pub(super) fn internal(error: impl std::fmt::Display) -> AppFail {
    fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
}

pub(super) type AppResult<T> = Result<T, AppFail>;

pub(super) fn load_config(state: &AppState) -> AppResult<live_core::config::Config> {
    live_core::config::load_config(&state.config_path)
        .map_err(|error| fail(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))
}

/// 数据根统一落点：config.output_dir。
pub(super) fn data_root(state: &AppState) -> AppResult<PathBuf> {
    Ok(load_config(state)?.output_dir)
}

/// JSON 体信封化——所有 JsonRejection（含 DefaultBodyLimit 触发的 413）
/// 统一落成 {error} JSON 响应，保持统一错误形态；原生 axum 只会吐裸文本。
pub(super) struct JsonBody<T>(pub T);

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
        .route("/rooms", get(rooms::rooms_list))
        .route("/rooms/:uid/overview", get(rooms::room_overview))
        .route("/rooms/:uid/viewers", get(rooms::room_viewers))
        .route("/rooms/:uid/viewers/:vid/tree", get(rooms::viewer_tree))
        .route(
            "/rooms/:uid/viewers/:vid/graph",
            get(graph_routes::viewer_graph),
        )
        .route("/rooms/:uid/graph", get(graph_routes::room_graph))
        .route(
            "/rooms/:uid/leads/:lead_id/approve",
            axum::routing::post(rooms::lead_approve),
        )
        .route(
            "/rooms/:uid/leads/:lead_id/reject",
            axum::routing::post(rooms::lead_reject),
        )
        .route(
            "/rooms/:uid/auto-collect",
            axum::routing::post(rooms::auto_collect_toggle),
        )
        .route(
            "/rooms/:uid/maintenance/entity_split",
            axum::routing::post(maintenance::maintenance_entity_split),
        )
        .route(
            "/rooms/:uid/maintenance/entity_merge",
            axum::routing::post(maintenance::maintenance_entity_merge),
        )
        .route("/config", get(config_routes::config_get))
        .route("/budget", get(config_routes::budget_get))
        .route("/runs", axum::routing::post(runs::runs_post))
        .route("/runs/:id", get(runs::run_get))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state.clone());

    let router = Router::new().nest("/api", api);
    // ServeDir 指向 web/dist；缺 dist → `/` 显示构建指引页（防静默 404）。
    // 消费 vite 预压缩产物——按 Accept-Encoding 协商 .br/.gz，无则回落原文件。
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

/// 缺 dist 时的构建指引（不静默 404）。
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

/// vid/viewer_uid 共用合法字符集——alnum + "_" + "-"。B 站 uid 是数字串，
/// demo uid 走「demo-N」形——制表与连字符之外一律视作穿透恶意（%2F 经 axum
/// 解码后落此；写侧 pane 同集守卫是纵深一对）。
pub fn uid_charset_legal(id: &str, max_len: usize) -> bool {
    !id.is_empty()
        && id.len() <= max_len
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub(super) fn vid_guard(vid: &str) -> AppResult<()> {
    if !uid_charset_legal(vid, MAX_VID_PATH_CHARS) {
        return Err(fail(StatusCode::NOT_FOUND, &format!("观众 {vid} 不存在")));
    }
    Ok(())
}

/// uid 守卫：路径形承诺——现布局单房间，uid 暂恒等于 config 房号，其他值一律 404。
pub(super) fn room_guard(config: &live_core::config::Config, uid: &str) -> AppResult<()> {
    if config.bilibili.room_id != uid {
        return Err(fail(
            StatusCode::NOT_FOUND,
            &format!("room {uid} 不存在（单房间布局）"),
        ));
    }
    Ok(())
}

/// graph 文件存在才开库（Store::open 会建文件，纯读路径禁止写入副作用）。
pub(super) fn open_graph(root: &std::path::Path) -> Option<live_core::graph::store::Store> {
    let path = root.join("graph").join("perception.sqlite3");
    path.exists()
        .then(|| live_core::graph::store::Store::open(&path).ok())
        .flatten()
}

// ---------------------------------------------------------------------------
// server 启动（serve/run 共用面）
// ---------------------------------------------------------------------------

pub struct StartOptions {
    pub config_path: PathBuf,
    pub port: u16,
    pub web_root: PathBuf,
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
            bilibili_hosts: options.bilibili_hosts,
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
