//! run 状态落盘面（M4-A `aggregate_runtime_usage` + state.json 形状 + 兜底伞）。
//!
//! parity 注记：usage 五键 Python 键序（llm_requests←llm_calls 重映射，其余同名）；
//! failed/interrupted 七键 state、run_id=None 跳过 fail_run 等语义逐字对齐
//! Python `except BaseException` 段。prompt-cache 计量以姊妹键 `cache_usage`
//! 落盘——Rust 增量面，不冒充 Python 输出。

use std::path::Path;

use serde_json::{Map, Value, json};

use super::run::graph_file;
use crate::config::Config;
use crate::graph::store::Store;
use crate::storage;

// ---------------------------------------------------------------------------
// aggregate_runtime_usage（Python ai_data.aggregate_runtime_usage）
// ---------------------------------------------------------------------------

/// 五键重映射求和：llm_requests←llm_calls，其余同名；Python 键序。
pub fn aggregate_runtime_usage(viewer_runtime: &[Value], overall_runtime: &Value) -> Value {
    let sum_of = |source_key: &str| -> i64 {
        viewer_runtime
            .iter()
            .map(|item| item.get(source_key).and_then(Value::as_i64).unwrap_or(0))
            .sum::<i64>()
            + overall_runtime
                .get(source_key)
                .and_then(Value::as_i64)
                .unwrap_or(0)
    };
    json!({
        "llm_requests": sum_of("llm_calls"),
        "tool_calls": sum_of("tool_calls"),
        "input_tokens": sum_of("input_tokens"),
        "output_tokens": sum_of("output_tokens"),
        "total_tokens": sum_of("total_tokens"),
    })
}

// ---------------------------------------------------------------------------
// 落盘形状（内部件）
// ---------------------------------------------------------------------------

/// analysis 落盘剥「空 leads」——Python 模型 extra=forbid：键存在即拒；
/// 空期双通（Rust serde 有 default 补齐、Python 无解码阻力）；非空 leads 是 M4.x
/// 新能力，跨实现缓存复用本就有界（登记 design-Δ）。
/// 空 front_brief（sentences 空数组）同型剥键——沉默以「键缺席」落盘，
/// 前端 BriefingCard 依「缺席必可见」呈空缺位。
pub(crate) fn strip_empty_leads(mut analysis: Value) -> Value {
    if analysis
        .get("leads")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        analysis
            .as_object_mut()
            .expect("analysis object")
            .remove("leads");
    }
    let brief_empty = analysis
        .get("front_brief")
        .and_then(|brief| brief.get("sentences"))
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    if brief_empty {
        analysis
            .as_object_mut()
            .expect("analysis object")
            .remove("front_brief");
    }
    analysis
}

/// 缓存/runtime 载荷只落 Python 五键；tool_names 属进程内诊断。
pub(crate) fn stats_json(stats: &super::super::runtime::RuntimeStats) -> Value {
    json!({
        "llm_calls": stats.llm_calls,
        "tool_calls": stats.tool_calls,
        "input_tokens": stats.input_tokens,
        "output_tokens": stats.output_tokens,
        "total_tokens": stats.total_tokens,
    })
}

/// cache 观测盒落地：usage 键守 Python 五键 parity；
/// prompt-cache 计量以姊妹键 `cache_usage` 进 state.json/final_result——
/// Rust 增量面，不冒充 Python 输出（零值如实落：复用臂/非 DeepSeek 端皆可读）。
pub(crate) fn cache_usage_json(viewer: (i64, i64), audience: (i64, i64)) -> Value {
    json!({
        "cache_hit_tokens": viewer.0 + audience.0,
        "cache_miss_tokens": viewer.1 + audience.1,
    })
}

pub(crate) fn write_state(path: &Path, fields: Value) -> Result<(), super::run::PipelineError> {
    storage::write_json(path, &fields).map_err(super::run::PipelineError::Storage)
}

pub(crate) fn utc_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// Python except BaseException 段（兜底伞）：fail_run + failed/interrupted 七键 state。
/// run_id=None 时跳过 fail_run（Python：graph_repo=None 不落 fail 行，state 照写）。
pub(crate) fn fail_run_and_state(
    config: &Config,
    state_path: &Path,
    viewer_input_hashes: &Map<String, Value>,
    run_id: Option<&str>,
    error: &str,
    viewer_stage_complete: bool,
    interrupted: bool,
) {
    if let Some(run_id) = run_id
        && let Ok(store) = Store::open(&graph_file(&config.output_dir))
    {
        // aborted=非 Exception 型：Rust 一切 Err 皆「Exception 等价」→ false；
        // 唯一 true 的来源是 ctrl-c（KeyboardInterrupt 同型物）。
        let _ = store.fail_run(run_id, error, interrupted);
    }
    let _ = write_state(
        state_path,
        json!({
            "status": if interrupted { "interrupted" } else { "failed" },
            "viewer_stage_status": if viewer_stage_complete { "complete" } else { "incomplete" },
            "updated_at": utc_now(),
            "model": config.ai.model,
            "error": error,
            "viewer_input_hashes": viewer_input_hashes,
            "graph_run_id": run_id,
        }),
    );
}

/// 兜底伞状态便签：inner 逐段点亮，outer 在任意 Err 上完成 Python except 段的收场。
#[derive(Default)]
pub(crate) struct UmbrellaNote {
    pub(crate) run_id: Option<String>,
    pub(crate) viewer_stage_complete: bool,
    pub(crate) viewer_input_hashes: Map<String, Value>,
}
