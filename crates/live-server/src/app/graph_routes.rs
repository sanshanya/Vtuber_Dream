//! 图端点：viewer graph + 整体图谱（Z6/P0-6 物化折叠视图 + ETag/304 协商）。
//!
//! 自 `app.rs` 按头注 rooms/config/runs 条款拆出（r8-F2 兑现）；sync DB/SQL 重活
//! 收 spawn_blocking 的原姿态不动（轮2-R1-B2），路径零变化。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::Value;

use super::{
    AppFail, AppResult, AppState, data_root, fail, internal, load_config, open_graph, room_guard,
    vid_guard,
};

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
    .map_err(internal)?;
    Ok((store, value))
}

pub(super) async fn viewer_graph(
    State(state): State<AppState>,
    Path((uid, vid)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    room_guard(&config, &uid)?;
    vid_guard(&vid)?;
    let root = data_root(&state)?;
    // 轮2-R1-B2：project() 同步 rusqlite（s0 全量 ≈0.6s）——spawn_blocking，
    // 与 compute_graph_bytes 同款姿态；守卫留在 async 面（微秒级）。
    let elements = tokio::task::spawn_blocking(move || -> AppResult<Value> {
        let (_store, value) = project_for_viewer(&root)?;
        Ok(crate::cytoscape::scoped(&value, &format!("viewer:{vid}")))
    })
    .await
    .map_err(internal)??;
    Ok(Json(elements))
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
    .map_err(internal)?
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
        let etag = crate::graph_artifact::content_probe(
            &store,
            &kinds_csv,
            crate::graph_artifact::GRAPH_FOLD_VERSION,
        )
        .map_err(internal)?;
        if let Some(artifact) = crate::graph_artifact::read_artifact(&root, &etag) {
            return Ok(artifact);
        }
        let _guard = lock
            .lock()
            .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "graph artifact 锁中毒"))?;
        // 双查：等待锁期间另一线程可能已重建（重探 = 同店 scan，零成本）。
        let etag2 = crate::graph_artifact::content_probe(
            &store,
            &kinds_csv,
            crate::graph_artifact::GRAPH_FOLD_VERSION,
        )
        .map_err(internal)?;
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
        .map_err(internal)?;
        drop(store);
        let folded = crate::cytoscape::elements_expanded(&value, &expanded);
        let artifact =
            crate::graph_artifact::write_artifact(&root, &etag2, folded.to_string().as_str())
                .map_err(internal)?;
        Ok(artifact)
    })
    .await
    .map_err(internal)?
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

pub(super) async fn room_graph(
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
