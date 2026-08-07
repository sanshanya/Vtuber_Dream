//! /api/config 面（打码读）。写面已删（删码刀4）——唯一操作者直接编辑
//! config.yaml 重启，PUT 白名单改写/原子写盘/写锁随行同删；读面保留给面板/调试。

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use super::{AppResult, AppState, load_config};

/// 主钮旁预估的墙钟粗估常量（秒）。纯体验提示非承诺——实测数据出现后校准：
/// 单人含 AI 段墙钟的 lo/hi 带宽，加 audience 段固定 90s 底（22 人 → 17~35 分钟）。
pub const PER_VIEWER_WALL_SECS_LO: u64 = 40;
pub const PER_VIEWER_WALL_SECS_HI: u64 = 90;
pub const AUDIENCE_WALL_SECS_BASE: u64 = 90;

pub(super) async fn config_get(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    let ai = &config.ai;
    let agent = &ai.agent;
    // cookie/api_key 只回存在性布尔，永不回显原文。
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
                "run_retries": agent.run_retries,
                "retry_backoff_seconds": agent.retry_backoff_seconds,
                "local_trace": agent.local_trace,
            },
            "search_results_per_query": ai.search_results_per_query,
            "max_output_tokens": ai.max_output_tokens,
            "rules": ai.rules,
            // 第 5 白名单键回显（None=不设闸，前端输入框初始为空）。
            "run_budget_cny": ai.run_budget_cny,
        },
    })))
}

/// GET /api/budget —— 薄预估面（删码刀3 收口）：预算闸现值 + 主钮旁预估。
///
/// estimate 段：roster（名册总人数）与 fresh（输入哈希已变 ∪ 无完整旧结论——
/// 与运行内预算闸唯一同源 pipeline::roster_estimate）、闸同公式预估 CNY、
/// 常量墙钟带宽带宽→分钟。名册/baseline 缺 → 全 null（前端「预估 —」不臆造）。
/// 月耗账本（history.jsonl）已删：实耗真相在 ai/state.json 的 usage 键，
/// 月度对账请用平台计费后台（本地不再维护第二账源）。
pub(super) async fn budget_get(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    let estimate = match live_core::agent::pipeline::roster_estimate(&config) {
        Some((fresh, total)) => {
            let estimated_cny = live_core::agent::budget::estimate_run_cost_cny(fresh, true);
            // 分钟数向上取整，避免把「余下的不足一分钟」低估成已完成。
            let secs_to_min = |secs: u64| secs.div_ceil(60);
            let lo = secs_to_min(fresh as u64 * PER_VIEWER_WALL_SECS_LO + AUDIENCE_WALL_SECS_BASE);
            let hi = secs_to_min(fresh as u64 * PER_VIEWER_WALL_SECS_HI + AUDIENCE_WALL_SECS_BASE);
            json!({
                "roster_viewers": total,
                "fresh_viewers": fresh,
                "estimated_cny": estimated_cny,
                "etd_minutes": [lo, hi],
            })
        }
        None => json!({
            "roster_viewers": null,
            "fresh_viewers": null,
            "estimated_cny": null,
            "etd_minutes": null,
        }),
    };
    Ok(Json(json!({
        "budget_cny": config.ai.run_budget_cny,
        "estimate": estimate,
    })))
}
