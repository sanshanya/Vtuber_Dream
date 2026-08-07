//! /api/config 面（打码读 + 白名单原子写）。
//!
//! 自 `app.rs` 按头注 rooms/config/runs 条款拆出；两个公共常量经
//! 根卷 `pub use` re-export，`app::WRITABLE_CONFIG_KEYS` 等外部路径零变化。

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{AppResult, AppState, JsonBody, fail, load_config};

/// PUT 单值长度上限。
pub const MAX_PUT_VALUE_CHARS: usize = 4096;
/// 主钮旁预估的墙钟粗估常量（秒）。纯体验提示非承诺——实测数据出现后校准：
/// 单人含 AI 段墙钟的 lo/hi 带宽，加 audience 段固定 90s 底（22 人 → 17~35 分钟）。
pub const PER_VIEWER_WALL_SECS_LO: u64 = 40;
pub const PER_VIEWER_WALL_SECS_HI: u64 = 90;
pub const AUDIENCE_WALL_SECS_BASE: u64 = 90;
/// 允许的写入键白名单（(顶层段, 键)）——此后扩展需要同名加键 + 测试。
pub const WRITABLE_CONFIG_KEYS: [(&str, &str); 5] = [
    ("bilibili", "cookie"),
    ("ai", "api_key"),
    ("ai", "base_url"),
    ("ai", "model"),
    ("ai", "run_budget_cny"),
];

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
                "max_turns": agent.max_turns,
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
        "writable_keys": WRITABLE_CONFIG_KEYS.iter().map(|(s, k)| format!("{s}.{k}")).collect::<Vec<_>>(),
    })))
}

/// 线路级 YAML 重写（保留注释与其余内容）——把 (段, 键) 定位段内首行
/// `  {key}:` 与整行替换为新值；找不到合法落点 → 422。
///
/// 加固三面：
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
                // rename 失败同样清走 tmp；错文不透服务器绝对路径。
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

pub(super) async fn config_put(
    State(state): State<AppState>,
    JsonBody(body): JsonBody<Value>,
) -> AppResult<Json<Value>> {
    let object = match body.as_object() {
        Some(object) => object,
        None => {
            return Err(fail(
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
            // 显式类型：非字符串值 → 422，不许被「空串=保持」沉默吞掉。
            if !value.is_string() {
                rejected.push(format!(
                    "{section}.{key} 的值必须是字符串（null→删除语义不支持）"
                ));
                continue;
            }
            let value_str = value.as_str().unwrap_or_default().to_string();
            match (writable, value_str.trim().is_empty()) {
                (Some(_), true) => {} // 空串 = 保持现值——语义只对白名单键生效
                // 错键+空串不得静默绕过 422 闸——
                // 拼写错误必须被点名，不得借「空串保持」滑走。
                (None, _) => rejected.push(format!("{section}.{key} 不在可写白名单")),
                (Some((s, k)), false) => {
                    // (Some(_), true) 已被上一臂吃掉——本臂只剩非空值进入重写。
                    if value_str.chars().count() > MAX_PUT_VALUE_CHARS {
                        return Err(fail(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            &format!("{s}.{k} 超出长度上限 {MAX_PUT_VALUE_CHARS}"),
                        ));
                    }
                    patch.push(((s, k), value_str));
                }
            }
        }
    }
    if !rejected.is_empty() {
        return Err(fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("拒绝的键：{}", rejected.join(", ")),
        ));
    }
    if patch.is_empty() {
        return Ok(Json(json!({"status": "unchanged"})));
    }
    // read-modify-write 全程持锁，并发 PUT 不得相互覆盖。write_keys 是同步块，
    // 持锁窗口不跨 await；锁中毒 = 上一次持锁者已 panic，用 expect 露头而非封锁修复。
    let _write_guard = state
        .config_write_lock
        .lock()
        .expect("config write lock poisoned");
    // 校验已并入 write_keys 的 tmp 阶段：失败 → 422 且原文件分毫未动。
    if let Err(error) = write_keys(&state.config_path, &patch) {
        return Err(fail(StatusCode::UNPROCESSABLE_ENTITY, &error));
    }
    Ok(Json(json!({"status": "updated", "keys": patch.len()})))
}

/// GET /api/budget —— 月度实耗汇总（读 `{output_dir}/history.jsonl`，
/// 每 run 终态一行，见 registry::append_history_line）。
///
/// 口径：只按追加序逐行聚合，坏行直接跳过（容忍记账面手改/半截写）；
/// 月界 = ts 前 7 字符 "YYYY-MM"（ISO 时间戳 +00:00，前缀比较即 UTC 月界，
/// 与中国时区 +8 的「自然月」不同，刻意选 UTC——账本口径前后端一致不动摇）；
/// 文件缺失/全坏 → 全零 + last_run=null。`budget_cny` = config 预算（None → null）。
///
/// `estimate` 段 = 主钮旁预估（normal_cny 走 budget::estimate_run_cost_cny
/// 上限口径、briefing_cny 恰 audience 平段、etd_minutes 为常量带宽→分钟）；名册
/// 缺/空 → 四个字段全 null（前端「预估 —」不臆造）。
pub(super) async fn budget_get(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let config = load_config(&state)?;
    let records: Vec<Value> = std::fs::read_to_string(config.output_dir.join("history.jsonl"))
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect()
        })
        .unwrap_or_default();
    // UTC 月界：ts 前 7 字符即 "YYYY-MM"。
    let month = &live_core::episodes::now_iso()[..7];
    let (month_cost_cny, month_runs) =
        records
            .iter()
            .fold((0.0_f64, 0_i64), |(cost, count), record| {
                if record["ts"]
                    .as_str()
                    .is_some_and(|ts| ts.starts_with(month))
                {
                    (cost + record["cost_cny"].as_f64().unwrap_or(0.0), count + 1)
                } else {
                    (cost, count)
                }
            });
    let last_run = records.last().map(|record| {
        json!({
            "run_id": record["run_id"],
            "ts": record["ts"],
            "cost_cny": record["cost_cny"],
            "status": record["status"],
            "kind": record["kind"],
            "spend_mode": record["spend_mode"],
        })
    });
    // 主钮旁预估 = 名册口径（collection.json 的 viewer_count）+ 闸同公式上限价 +
    // 墙钟粗估带宽。名册缺文件/非数/为空 → estimate 全 null（前端落「预估 —」不臆造）。
    let roster_viewers = live_core::storage::read_json(&config.output_dir.join("collection.json"))
        .ok()
        .flatten()
        .and_then(|collection| collection["viewer_count"].as_i64());
    let estimate = match roster_viewers {
        Some(count) if count > 0 => {
            let normal_cny = live_core::agent::budget::estimate_run_cost_cny(count as usize, true);
            let briefing_cny = live_core::agent::budget::estimate_run_cost_cny(0, true);
            // 分钟数向上取整，避免把「余下的不足一分钟」低估成已完成。
            let secs_to_min = |secs: u64| secs.div_ceil(60);
            let lo = secs_to_min(count as u64 * PER_VIEWER_WALL_SECS_LO + AUDIENCE_WALL_SECS_BASE);
            let hi = secs_to_min(count as u64 * PER_VIEWER_WALL_SECS_HI + AUDIENCE_WALL_SECS_BASE);
            json!({
                "roster_viewers": count,
                "normal_cny": normal_cny,
                "briefing_cny": briefing_cny,
                "etd_minutes": [lo, hi],
            })
        }
        _ => json!({
            "roster_viewers": null,
            "normal_cny": null,
            "briefing_cny": null,
            "etd_minutes": null,
        }),
    };
    Ok(Json(json!({
        "budget_cny": config.ai.run_budget_cny,
        "month": month,
        "month_cost_cny": month_cost_cny,
        "month_runs": month_runs,
        "last_run": last_run,
        "estimate": estimate,
    })))
}
