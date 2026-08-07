//! 整体态势面（M4-A `compact_interest_state` / `build_audience_input` 两级封顶闸门，
//! 加上 M4-C `_run_audience` 阶段体）。golden 对账：tests/pipeline_inputs.rs ×
//! tests-fixtures/m4a/（经 mod 壳再导出，坐标 `live_core::agent::pipeline::*` 不变）。

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::super::prompts::{audience_user_prompt, trace_run_start};
use super::super::runtime::{AgentRuntime, AttemptPlan, Trace, run_toolcall_agent};
use super::super::specs::audience_agent_spec;
use super::super::tools::{AudienceAgentCtx, ResearchService};
use super::super::validators::validate_audience_submission;
use super::cache::{CACHE_RUNTIME_AUDIENCE, complete_cache, reasoning_json, stable_hash};
use super::run::{PipelineError, PipelineKnobs, graph_file, ledger_annex, progress_say};
use super::state::{stats_json, strip_empty_leads};
use crate::config::Config;
use crate::graph::store::Store;
use crate::models::AudienceSituationSubmission;
use crate::storage;

/// Python `tools.AUDIENCE_PROFILE_SUMMARY_MAX_CHARS`（字符数，非字节）。
pub const AUDIENCE_PROFILE_SUMMARY_MAX_CHARS: usize = 2_000;
/// Python `tools.AUDIENCE_INITIAL_CONTEXT_MAX_CHARS`（字符数，非字节）。
pub const AUDIENCE_INITIAL_CONTEXT_MAX_CHARS: usize = 500_000;

// ---------------------------------------------------------------------------
// compact_interest_state（Python tools._compact_interest_state）
// ---------------------------------------------------------------------------

const COMPACT_STATE_KEYS: [&str; 10] = [
    "entity_ref",
    "entity_id",
    "entity",
    "canonical_name",
    "status",
    "preference",
    "aspects",
    "rationale",
    "evidence_mention_ids",
    "confidence",
];

/// Python `state.get(key) not in (None, "", [])`：空对象 {} 保留、0/0.0/False 保留。
pub fn compact_interest_state(state: &Value) -> Value {
    let mut out = Map::new();
    for key in COMPACT_STATE_KEYS {
        match state.get(key) {
            Some(value @ (Value::Bool(_) | Value::Number(_) | Value::Object(_))) => {
                out.insert(key.to_string(), value.clone());
            }
            Some(value @ Value::String(_)) if !value.as_str().unwrap_or_default().is_empty() => {
                out.insert(key.to_string(), value.clone());
            }
            Some(value @ Value::Array(_)) if !value.as_array().unwrap_or(&vec![]).is_empty() => {
                out.insert(key.to_string(), value.clone());
            }
            _ => {}
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// build_audience_input（Python tools.build_audience_input：索引 + 两级封顶）
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AudienceInputError {
    /// Python `ValueError("audience index exceeds bounded initial context")`。
    #[error("audience index exceeds bounded initial context")]
    TooLarge,
}

/// Python `str(value or "")`（falsy → ""）；预算/索引专用（0/0.0 落槽语义与 Python 对齐）。
/// 互指：agent/tools.rs 的 py_or_empty 是完整 falsy 判定 + other→py_str 臂
/// （工具面要承载 array/object）；本件只喂标量槽（预算/索引），_ 臂直接落 "" 是
/// 刻意的窄口径，两边职责不同，禁止合并。
fn or_empty(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Number(n)) if n.as_f64().unwrap_or(0.0) != 0.0 => n.to_string(),
        _ => String::new(),
    }
}

fn counter_add(map: &mut Map<String, Value>, key: &str) {
    let next = map.get(key).and_then(Value::as_i64).unwrap_or(0) + 1;
    map.insert(key.to_string(), json!(next));
}

/// 紧凑序列化的**字符数**（Python `len(json.dumps(..., separators=(",",":")))` 是码点数）。
fn compact_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .count()
}

/// Python `build_audience_input`：bounded 索引；详情只经工具按需取。两级封顶：
/// ①逐条回退 interest_state 直到预算内；②清空全部 profile_summary；③仍超 → TooLarge。
/// 语义哈希口径（audience 侧，与 episodes_hash_material 同族）：baseline_summary 的
/// collection_request_count / collection_elapsed_seconds 是**过程指标**（每轮采集必变），
/// 不是听众事实——计入哈希会让 situation 缓存跨采集恒假死。提示面原样保留（与 Python
/// 预言机 golden 对账面不漂移），哈希件剔除这两键。
/// oracle 测试面需要直接消费——与 build_audience_input 同族升级为 pub。
pub fn audience_input_hash_material(input: &Value) -> Value {
    let mut material = input.clone();
    if let Value::Object(summary) = &mut material["baseline_summary"] {
        summary.remove("collection_request_count");
        summary.remove("collection_elapsed_seconds");
    }
    // 同款戒条：platform_snapshot.captured_at = 本轮采集墙钟，非平台快照的事实面。
    if let Value::Object(snapshot) = &mut material["platform_snapshot"] {
        snapshot.remove("captured_at");
    }
    material
}

pub fn build_audience_input(
    analysis: &Value,
    viewer_analyses: &Map<String, Value>,
    graph: &Value,
) -> Result<Value, AudienceInputError> {
    let mut viewer_names: Map<String, Value> = Map::new();
    for item in analysis
        .get("viewer_profiles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if item.is_object() {
            let viewer = &item["viewer"];
            viewer_names.insert(
                or_empty(viewer.get("id")),
                json!(or_empty(viewer.get("name"))),
            );
        }
    }
    let name_of = |viewer_id: &str| -> String {
        match viewer_names.get(viewer_id).and_then(Value::as_str) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => viewer_id.to_string(),
        }
    };
    let mut node_types = Map::new();
    for node in graph
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let kind = node
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let kind = if kind.is_empty() { "unknown" } else { kind };
        counter_add(&mut node_types, kind);
    }
    let mut predicates = Map::new();
    for edge in graph
        .get("edges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let kind = edge
            .get("predicate")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let kind = if kind.is_empty() { "unknown" } else { kind };
        counter_add(&mut predicates, kind);
    }
    let mut episode_counts = Map::new();
    for node in graph
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if node.get("type").and_then(Value::as_str) == Some("Episode") {
            let viewer_id = or_empty(node["properties"].get("viewer_id"));
            counter_add(&mut episode_counts, &viewer_id);
        }
    }
    let mut state_candidates: Vec<Value> = Vec::new();
    let mut state_counts = Map::new();
    for state in graph
        .get("interest_states")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if !state.is_object() {
            continue;
        }
        let viewer_id = or_empty(state.get("viewer_id"));
        if viewer_id.is_empty() {
            continue;
        }
        let compact = compact_interest_state(&state);
        let mut entry = json!({"viewer_id": viewer_id, "viewer_name": name_of(&viewer_id)});
        if let (Value::Object(base), Value::Object(extra)) = (&mut entry, compact) {
            for (key, value) in extra {
                base.insert(key, value);
            }
        }
        counter_add(&mut state_counts, &viewer_id);
        state_candidates.push(entry);
    }
    let count_of = |map: &Map<String, Value>, key: &str| -> i64 {
        map.get(key).and_then(Value::as_i64).unwrap_or(0)
    };
    let mut viewer_index: Vec<Value> = Vec::new();
    for (viewer_id, item) in viewer_analyses {
        let summary = or_empty(item.get("profile_summary"));
        let clipped: String = summary
            .chars()
            .take(AUDIENCE_PROFILE_SUMMARY_MAX_CHARS)
            .collect();
        viewer_index.push(json!({
            "viewer_id": viewer_id,
            "viewer_name": name_of(viewer_id),
            "profile_summary": clipped,
            "episode_count": count_of(&episode_counts, viewer_id),
            "mention_count": item.get("mentions").and_then(Value::as_array).map_or(0, Vec::len) as i64,
            "entity_count": item.get("entities").and_then(Value::as_array).map_or(0, Vec::len) as i64,
            "interest_state_count": count_of(&state_counts, viewer_id),
        }));
    }
    let mut payload = json!({
        "baseline_summary": analysis.get("summary").cloned().unwrap_or(json!({})),
        "platform_snapshot": analysis.get("platform_snapshot").cloned().unwrap_or(json!({})),
        "streamer": analysis.get("streamer").cloned().unwrap_or(json!({})),
        "viewer_index": viewer_index,
        "interest_state_index": Vec::<Value>::new(),
        "graph_index": {
            "stats": graph.get("stats").cloned().unwrap_or(json!({})),
            "node_type_counts": Value::Object(node_types),
            "predicate_counts": Value::Object(predicates),
            "communities": graph.get("communities").cloned().unwrap_or(json!([])),
        },
        "detail_access": {
            "viewer_tool": "get_viewer_analysis",
            "graph_tool": "query_graph",
            "instruction": "按需查询具体个人、实体、关系和Mention证据；不要无条件读取所有详情。",
        },
        "omitted_interest_state_count": 0,
    });
    // Python 逐条 push 后量预算、超即 pop 并 break；Rust 借用规则下改为「push → 回填 → 量」。
    let mut included: Vec<Value> = Vec::new();
    for state in state_candidates {
        included.push(state);
        payload["interest_state_index"] = json!(included);
        if compact_chars(&payload) > AUDIENCE_INITIAL_CONTEXT_MAX_CHARS {
            included.pop();
            break;
        }
    }
    payload["interest_state_index"] = json!(included);
    let included_len = payload["interest_state_index"]
        .as_array()
        .map_or(0, Vec::len);
    // omitted = 候选总数 − 纳入数；候选已消费为 state_counts，由其和重算。
    let candidate_count = state_count_sum(&state_counts);
    let omitted_count = candidate_count - included_len as i64;
    payload["omitted_interest_state_count"] = json!(omitted_count);
    if compact_chars(&payload) > AUDIENCE_INITIAL_CONTEXT_MAX_CHARS {
        for viewer in payload["viewer_index"]
            .as_array_mut()
            .expect("viewer_index is array")
        {
            viewer["profile_summary"] = json!("");
        }
    }
    if compact_chars(&payload) > AUDIENCE_INITIAL_CONTEXT_MAX_CHARS {
        return Err(AudienceInputError::TooLarge);
    }
    Ok(payload)
}

fn state_count_sum(state_counts: &Map<String, Value>) -> i64 {
    state_counts.values().filter_map(Value::as_i64).sum()
}

// ---------------------------------------------------------------------------
// run_audience_stage（Python `_run_audience` 阶段体；M4-C）
// ---------------------------------------------------------------------------

/// 整体态势阶段（Python `_run_audience`）：输入 → hash → 缓存恢复 → agent → 落盘。
/// research 按值进/出（含 blocking client——处置集中在 run 卷的编排层 spawn_blocking）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_audience_stage(
    analysis: &Value,
    viewer_map: &Map<String, Value>,
    graph_context: &Value,
    research: ResearchService,
    config: &Config,
    runtime: &AgentRuntime,
    knobs: &PipelineKnobs<'_>,
    force: bool,
    // 三元组尾件 = 本轮 LLM 的 prompt-cache 计量 (hit, miss)；复用臂为 (0, 0)。
) -> (
    Result<(Value, Value, (i64, i64)), PipelineError>,
    ResearchService,
) {
    let input = match build_audience_input(analysis, viewer_map, graph_context) {
        Ok(input) => input,
        Err(err) => {
            return (Err(PipelineError::Message(err.to_string())), research);
        }
    };
    // 哈希件走 audience_input_hash_material（过程指标摘出——同数据重采同哈希），
    // 提示面 input 原样保真（golden 对账不漂移）。
    // 协议版本串入哈希——终局 schema 或指令文本一改，situation 缓存即失效
    // （认知层正确性条款：否则新字段如 front_brief 永不被补算）。viewer 面另有
    // reasoning/rules 成分但暂未入版本串——登记为统一化设计债。
    let input_hash = stable_hash(&json!({
        "runtime": CACHE_RUNTIME_AUDIENCE,
        "model": config.ai.model,
        "api": config.ai.api,
        "reasoning": reasoning_json(config),
        "rules": config.ai.rules,
        "prompts_version": super::super::prompts::PROMPTS_VERSION,
        "tool_specs_version": super::super::specs::TOOL_SPECS_VERSION,
        "input": audience_input_hash_material(&input),
    }));
    let cache_path = config.output_dir.join("ai").join("situation.json");
    if !force && config.ai.agent.resume {
        let cached = storage::read_json(&cache_path).ok().flatten();
        if let Some(cached) = cached.as_ref() {
            if !complete_cache(cached, &input_hash) {
                // 观测面：situation 缓存命中判别不可静默——哈希不等 / 复核不过 / 形态坏
                // 三分面都写 events（viewer 阶段同款静默曾让我们排查三小时）。
                let shape_ok = serde_json::from_value::<AudienceSituationSubmission>(
                    cached["analysis"].clone(),
                )
                .is_ok();
                progress_say(
                    knobs,
                    &format!(
                        "[AI] situation 缓存未命中（hash 不等或形态坏：analysis 解析={shape_ok}）→ 重跑 audience"
                    ),
                );
            } else if let Ok(submission) =
                serde_json::from_value::<AudienceSituationSubmission>(cached["analysis"].clone())
            {
                let entity_ids: HashSet<String> = graph_context["nodes"]
                    .as_array()
                    .map(|nodes| {
                        nodes
                            .iter()
                            .filter(|n| n["type"] == "Entity" && n["id"].is_string())
                            .filter_map(|n| n["id"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let mention_ids: HashSet<String> = graph_context["mentions"]
                    .as_array()
                    .map(|mentions| {
                        mentions
                            .iter()
                            .filter_map(|m| m["mention_id"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let viewer_ids: HashSet<String> = viewer_map.keys().cloned().collect();
                let search_ids: HashSet<String> = research.search_results.keys().cloned().collect();
                let errors = validate_audience_submission(
                    &submission,
                    &viewer_ids,
                    &entity_ids,
                    &mention_ids,
                    &search_ids,
                );
                if errors.is_empty() {
                    progress_say(knobs, "[AI] 复用整体Situation");
                    let runtime = cached
                        .get("runtime")
                        .filter(|v| v.is_object())
                        .cloned()
                        .unwrap_or(json!({}));
                    // 复用臂：本轮零 LLM 请求 → cache 计量诚实归零。
                    return (Ok((cached["analysis"].clone(), runtime, (0, 0))), research);
                }
                progress_say(
                    knobs,
                    &format!(
                        "[AI] situation 缓存复核未过：{} 项 → 重跑 audience",
                        errors.len()
                    ),
                );
            }
        }
    }
    progress_say(
        knobs,
        "[AI] 运行整体Situation Agent（全员索引 + 按需证据查询）",
    );
    let trace_path = config.ai.agent.local_trace.then(|| {
        config
            .output_dir
            .join("ai")
            .join("traces")
            .join("audience.jsonl")
    });
    let mut trace = Trace::new(trace_path);
    let mut spec = audience_agent_spec(&config.ai.rules);
    trace_run_start(
        &mut trace,
        &spec.name,
        &config.ai.model,
        "submit_audience_situation",
        "live_core::models::AudienceSituationSubmission",
    );
    // 装配纪律 1：audience ctx.graph_run_id 恒 None（Some 只属 Peer 链 G2）。
    let store = match Store::open(&graph_file(&config.output_dir)) {
        Ok(store) => store,
        Err(err) => return (Err(PipelineError::Store(err)), research),
    };
    let prompt = ledger_annex(&store, None, audience_user_prompt(&input));
    let mut ctx = AudienceAgentCtx {
        viewer_analyses: viewer_map.clone(),
        research,
        store,
        graph_run_id: None,
        slot: Default::default(),
    };
    let started = std::time::Instant::now();
    let outcome = run_toolcall_agent::<AudienceAgentCtx, AudienceSituationSubmission>(
        runtime,
        &mut spec,
        AttemptPlan {
            label: "整体Situation",
            prompt: &prompt,
            max_turns: usize::MAX,
            retries: config.ai.agent.run_retries as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
            // token 预算熔断只属 viewer 面；audience 单 agent 不设 budget。
            token_budget: None,
        },
        &mut ctx,
        &mut trace,
    )
    .await;
    let elapsed = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let runtime_payload = stats_json(&trace.stats);
    // 同 viewer 臂——cache 计量进程内 tally，不进 persisted runtime 五键。
    let cache_tally = (trace.stats.cache_hit_tokens, trace.stats.cache_miss_tokens);
    let AudienceAgentCtx { research, .. } = ctx;
    match outcome {
        Ok(outcome) => {
            let overall = serde_json::to_value(&outcome.submission).expect("submission 序列化");
            let write = storage::write_json(
                &cache_path,
                &json!({
                    "status": "complete",
                    "input_hash": input_hash,
                    "model": config.ai.model,
                    "protocol": "terminal_tool_call",
                    "terminal_tool": "submit_audience_situation",
                    "elapsed_seconds": elapsed,
                    "runtime": runtime_payload,
                    "analysis": strip_empty_leads(overall.clone()),
                }),
            )
            .map_err(PipelineError::Storage);
            (
                write.map(|()| (overall, runtime_payload, cache_tally)),
                research,
            )
        }
        Err(err) => {
            let _ = storage::write_json(
                &cache_path,
                &json!({
                    "status": "failed",
                    "input_hash": input_hash,
                    "model": config.ai.model,
                    "elapsed_seconds": elapsed,
                    "runtime": runtime_payload,
                    "error": err.to_string(),
                }),
            );
            (Err(PipelineError::Message(err.to_string())), research)
        }
    }
}
