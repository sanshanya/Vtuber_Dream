//! 图维护缝（G1 / design §8.6 行 224-229）：
//! POST /api/rooms/:uid/maintenance/entity_split|entity_merge。
//!
//! 自 `app.rs` 按头注 rooms/config/runs 条款拆出（r8-F2 兑现），路径零变化。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use live_core::graph::store::{MaintenanceError, Store as GraphStore};

use super::{
    AppFail, AppResult, AppState, JsonBody, data_root, fail, load_config, open_graph, room_guard,
};

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
pub(super) async fn maintenance_entity_split(
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
pub(super) async fn maintenance_entity_merge(
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
