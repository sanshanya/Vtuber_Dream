//! runs 面（B3）：POST 触发 + registry 轮询。
//!
//! 自 `app.rs` 按头注 rooms/config/runs 条款拆出；`MAX_VIEWER_UID_CHARS`
//! 经根卷 `pub use` re-export，`app::MAX_VIEWER_UID_CHARS` 外部路径零变化。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{AppResult, AppState, JsonBody, fail, load_config, uid_charset_legal};

/// POST runs 的 viewer_uid 长度上限（B 站 uid 为数字串，32 已富余）。
pub const MAX_VIEWER_UID_CHARS: usize = 32;

/// POST /api/runs {kind, force?, viewer_uid?} → 202 {run_id}。
///
/// 校验口径（kickoff B3b 冻结）：kind ∈ {full, viewer}；kind=viewer 必须给出
/// viewer_uid 且与 force 互斥（force 是全量清理语义）；kind=full 与 viewer_uid
/// 互斥；非布尔 force / 超长 uid / 非对象体一律 422。
pub(super) async fn runs_post(
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
            // 写侧穿透——collector 容错链以 input uid 落盘 viewers/{uid}.json，
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
        // 分层四 kind 是纯整体动作——不接 viewer_uid（单人=kind=viewer）。
        (kind, Some(_)) if crate::registry::RUN_KINDS_STAGED.contains(&kind) => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("kind={kind} 不接受 viewer_uid（单舰长动作 = kind=viewer）"),
            ));
        }
        // 分层四 kind 亦不接 force——force=全量清 AI 缓存语义仅属 kind=full；
        // ai_* 的幂等就是保留在Collect面的前提下做哈希失配重算。
        (kind, _) if crate::registry::RUN_KINDS_STAGED.contains(&kind) && force => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("kind={kind} 不接受 force（force 仅属 kind=full 的全量语义）"),
            ));
        }
        (_, viewer_uid) => viewer_uid,
    };
    // spend_mode 是省钱模式的可选键——缺席/空=Normal；只收字面
    // incremental / briefing_only（其余一律 422，复用 SpendMode::parse 文案），
    // 且只对带单人感知段的 kind 合法（full/viewer/ai_viewers）。
    let spend_mode = match object.get("spend_mode") {
        None | Some(Value::Null) => live_core::agent::budget::SpendMode::Normal,
        Some(Value::String(raw)) => live_core::agent::budget::SpendMode::parse(raw.trim())
            .map_err(|message| fail(StatusCode::UNPROCESSABLE_ENTITY, &message))?,
        Some(_) => {
            return Err(fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "spend_mode 只收 incremental / briefing_only",
            ));
        }
    };
    if spend_mode != live_core::agent::budget::SpendMode::Normal
        && !matches!(kind, "full" | "viewer" | "ai_viewers")
    {
        return Err(fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("kind={kind} 无单人感知段，spend_mode 无消费"),
        ));
    }
    // 已有未终态 run → 409，客户端可从错文中拿到在飞 run_id 自行轮询。
    let record = crate::registry::Registry::spawn_run(
        &state.registry,
        load_config(&state)?,
        kind,
        viewer_uid,
        force,
        spend_mode,
        state.bilibili_hosts.clone(),
    )
    .map_err(|active| {
        fail(
            StatusCode::CONFLICT,
            &format!("已有进行中的 run（{active}），待其到达终态后再触发"),
        )
    })?;
    let run_id = record.lock().expect("record poisoned").run_id.clone();
    Ok((StatusCode::ACCEPTED, Json(json!({"run_id": run_id}))))
}

pub(super) async fn run_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    match state.registry.get(&id) {
        Some(record) => Ok(Json(crate::registry::run_to_json(
            &record.lock().expect("record poisoned"),
        ))),
        None => Err(fail(StatusCode::NOT_FOUND, &format!("run {id} 不存在"))),
    }
}
