//! AudienceAnalysisAgent 编排（移植 `agent/pipeline.py`；Peer 链挂 G2）。
//!
//! M4-A 先落输入小件：`stable_hash` / `aggregate_runtime_usage` / `viewer_input_bundle` /
//! `compact_interest_state` / `build_audience_input`（两级封顶闸门）。
//! golden 对账：tests/pipeline_inputs.rs × tests-fixtures/m4a/（Python 实算）。

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::episodes::{self, Episode, baseline::viewer_context};

/// 缓存键 runtime 串（Python `_viewer_input_bundle` 逐字；跨实现缓存可比的锚，不得改字）。
pub const CACHE_RUNTIME_VIEWER: &str = "openai-agents-toolcall-grounded-v0.12-validated-cache";
/// 缓存键 runtime 串（Python `_run_audience` 逐字）。
pub const CACHE_RUNTIME_AUDIENCE: &str = "openai-agents-toolcall-situation-v0.12-validated-cache";

/// Python `tools.AUDIENCE_PROFILE_SUMMARY_MAX_CHARS`（字符数，非字节）。
pub const AUDIENCE_PROFILE_SUMMARY_MAX_CHARS: usize = 2_000;
/// Python `tools.AUDIENCE_INITIAL_CONTEXT_MAX_CHARS`（字符数，非字节）。
pub const AUDIENCE_INITIAL_CONTEXT_MAX_CHARS: usize = 500_000;

// ---------------------------------------------------------------------------
// stable_hash / canonical_json（Python ai_data.stable_hash）
// ---------------------------------------------------------------------------

/// Python `json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))`。
/// 语义绑定 episodes::json_canon（键按字节序 = unicode 码点序；ensure_ascii=False 为 serde_json 默认）。
pub fn canonical_json(value: &Value) -> String {
    episodes::json_canon(value)
}

/// Python `stable_hash`：canonical JSON 的 sha256 hexdigest。
pub fn stable_hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical_json(value).as_bytes()))
}

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
// viewer_input_bundle（Python pipeline._viewer_input_bundle）
// ---------------------------------------------------------------------------

pub struct ViewerInputBundle {
    pub context_data: Value,
    pub episodes: Vec<Episode>,
    pub input_payload: Value,
    pub input_hash: String,
}

/// Python `_viewer_input_bundle`：context + episodes + payload + hash 四元组。
/// `reasoning`/`rules`/`model`/`api` 只参与 hash，不改写平台事实。
pub fn viewer_input_bundle(
    raw_viewer: &Value,
    baseline: &Value,
    model: &str,
    api: &str,
    reasoning: &Value,
    rules: &[String],
    max_evidence_per_viewer: usize,
) -> ViewerInputBundle {
    let context_data = viewer_context(raw_viewer, baseline, max_evidence_per_viewer);
    let episodes = episodes::episodes_from_context(&context_data);
    let input_payload = json!({
        "viewer": context_data.get("viewer").cloned().unwrap_or(json!({})),
        "public_profile": context_data.get("public_profile").cloned().unwrap_or(json!({})),
        "source_statuses": context_data.get("source_statuses").cloned().unwrap_or(json!({})),
        "episodes": serde_json::to_value(&episodes).expect("Episode 恒可序列化"),
        "deterministic_mention_seeds": episodes::deterministic_mention_seeds(&episodes),
    });
    let input_hash = stable_hash(&json!({
        "runtime": CACHE_RUNTIME_VIEWER,
        "model": model,
        "api": api,
        "reasoning": reasoning,
        "rules": rules,
        "input": input_payload,
    }));
    ViewerInputBundle {
        context_data,
        episodes,
        input_payload,
        input_hash,
    }
}

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
// 阶段状态机（Python AudienceAnalysisAgent.run_async 平移；M4-C）
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::Semaphore;

use super::prompts::{audience_user_prompt, trace_run_start, viewer_user_prompt};
use super::runtime::{AgentRuntime, AttemptPlan, RuntimeStats, Trace, run_toolcall_agent};
use super::specs::{audience_agent_spec, viewer_agent_spec};
use super::tools::{AudienceAgentCtx, ResearchService, ViewerAgentCtx};
use super::validators::{validate_audience_submission, validate_viewer_submission};
use crate::bilibili::BilibiliClient;
use crate::config::Config;
use crate::graph::build;
use crate::graph::project::{AUDIENCE_GRAPH_LIMIT, ProjectOptions, project};
use crate::graph::store::{Store, StoreError};
use crate::models::{AudienceSituationSubmission, ViewerPerceptionSubmission};
use crate::storage::{self, load_viewers};

/// Python asyncio.Semaphore(4)（ADR-0004 同构）。
pub const INVESTIGATE_CONCURRENCY: usize = 4;

#[derive(Debug, Error)]
pub enum PipelineError {
    /// Python AgentRuntimeError 消息面 parity（文案逐字场景）。
    #[error("{0}")]
    Message(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("storage: {0}")]
    Storage(String),
}

impl From<String> for PipelineError {
    fn from(value: String) -> Self {
        PipelineError::Storage(value)
    }
}

type ApplyViewerFn<'a> = dyn FnMut(&Store, &str, &str, &[Episode], &ViewerPerceptionSubmission) -> Result<(), StoreError>
    + Send
    + 'a;

/// 可插接缝（不抽象化：各一个真实默认 + 测试覆盖点）。
#[derive(Default)]
pub struct PipelineKnobs<'a> {
    /// Python progress callback。
    pub progress: Option<&'a dyn Fn(&str)>,
    /// 每观众应用完成后的报告刷新（Python checkpoint=；M5 报告层真实接线）。
    pub checkpoint: Option<&'a mut dyn FnMut()>,
    /// 测试接缝：graph_failed 分支的确定性复现（默认 build::apply_viewer_submission）。
    pub apply_viewer: Option<&'a mut ApplyViewerFn<'a>>,
    /// 测试接缝：Bilibili 回放根地址（默认官方端点；调查工具发起网络时才消费）。
    pub bilibili_origin: Option<(String, String)>,
}

fn progress_say(knobs: &PipelineKnobs<'_>, message: &str) {
    if let Some(progress) = knobs.progress {
        progress(message);
    }
}

/// D-4：缓存/runtime 载荷只落 Python 五键；tool_names 属进程内诊断。
fn stats_json(stats: &RuntimeStats) -> Value {
    json!({
        "llm_calls": stats.llm_calls,
        "tool_calls": stats.tool_calls,
        "input_tokens": stats.input_tokens,
        "output_tokens": stats.output_tokens,
        "total_tokens": stats.total_tokens,
    })
}

fn write_state(path: &Path, fields: Value) -> Result<(), PipelineError> {
    storage::write_json(path, &fields).map_err(PipelineError::Storage)
}

fn reasoning_json(config: &Config) -> Value {
    json!({
        "enabled": config.ai.reasoning.enabled,
        "effort": config.ai.reasoning.effort,
        "replay_content": config.ai.reasoning.replay_content,
    })
}

fn graph_file(root: &Path) -> PathBuf {
    root.join("graph").join("perception.sqlite3")
}

fn new_client(
    config: &Config,
    origin: Option<(&str, &str)>,
) -> Result<BilibiliClient, PipelineError> {
    let (api, live) =
        origin.unwrap_or(("https://api.bilibili.com", "https://api.live.bilibili.com"));
    BilibiliClient::with_origin(
        api,
        live,
        &config.bilibili.cookie,
        config.collection.request_delay_seconds,
        config.collection.timeout_seconds,
    )
    .map_err(|err| PipelineError::Message(format!("bilibili client: {err}")))
}

/// `_complete_cache`：dict ∧ status=="complete" ∧ hash 相等 ∧ analysis 是 dict。
fn complete_cache(cache: &Value, input_hash: &str) -> bool {
    cache.is_object()
        && cache.get("status").and_then(Value::as_str) == Some("complete")
        && cache.get("input_hash").and_then(Value::as_str) == Some(input_hash)
        && cache.get("analysis").is_some_and(Value::is_object)
}

struct ViewerTaskOut {
    analysis: Value,
    /// 待 absorb 的子实例（含 blocking client——跨任务移动后由主任务集中处置）。
    child: ResearchService,
}

enum ViewerStage {
    Ok(Box<ViewerTaskOut>),
    /// 缓存复用短路（Python：不再触碰 research；无新发现可吸收）。
    Reused(Value),
    Failed,
}

/// 单观众：缓存恢复 → agent 运行 → 缓存落盘。所有阻塞件经 spawn_blocking（M3 blocker）。
#[allow(clippy::too_many_arguments)]
async fn run_one_viewer(
    uid: String,
    name: String,
    bundle: ViewerInputBundle,
    cache_path: PathBuf,
    trace_path: Option<PathBuf>,
    graph_file: PathBuf,
    config: Arc<Config>,
    runtime: Arc<AgentRuntime>,
    origin: Option<(String, String)>,
    force: bool,
) -> (String, ViewerStage) {
    let input_hash = bundle.input_hash.clone();
    let episodes: std::collections::BTreeMap<String, Episode> = bundle
        .episodes
        .iter()
        .map(|episode| (episode.episode_id.clone(), episode.clone()))
        .collect();
    // 缓存恢复（Python parity：complete+hash → model_validate → 当前闭包重校验）
    if !force
        && config.ai.agent.resume
        && let Some(cached) = storage::read_json(&cache_path).ok().flatten()
        && complete_cache(&cached, &input_hash)
        && let Ok(submission) =
            serde_json::from_value::<ViewerPerceptionSubmission>(cached["analysis"].clone())
    {
        let child_store = {
            let graph_file_ref = graph_file.clone();
            let uid_ref = uid.clone();
            let episodes_ref = episodes.clone();
            tokio::task::spawn_blocking(move || {
                let store = Store::open(&graph_file_ref).map_err(|err| err.to_string())?;
                let entity_exists =
                    |candidate: &str| store.entity_exists(candidate).unwrap_or(false);
                // Python：当前运行共享注册表；子实例起点 = 同磁盘视图（等价闭包）。
                let search_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                Ok::<Vec<String>, String>(validate_viewer_submission(
                    &submission,
                    &uid_ref,
                    &episodes_ref,
                    &entity_exists,
                    &search_ids,
                ))
            })
            .await
            .expect("spawn join")
        };
        if let Ok(errors) = child_store
            && errors.is_empty()
        {
            return (uid, ViewerStage::Reused(cached["analysis"].clone()));
        }
    }
    let context_data = bundle.context_data.clone();
    let input_payload = bundle.input_payload.clone();
    let rules = config.ai.rules.clone();
    let (build_graph_file, build_root, build_config, build_origin) = (
        graph_file.clone(),
        config.output_dir.clone(),
        config.clone(),
        origin.clone(),
    );
    let built = tokio::task::spawn_blocking(move || {
        let client = new_client(
            &build_config,
            build_origin.as_ref().map(|(a, l)| (a.as_str(), l.as_str())),
        )
        .map_err(|err| err.to_string())?;
        let research = ResearchService::new(
            &build_root,
            client,
            build_config.ai.search_results_per_query,
        )
        .with_persistence(false);
        let store = Store::open(&build_graph_file).map_err(|err| err.to_string())?;
        Ok::<(ResearchService, Store), String>((research, store))
    });
    let (research, store) = match built.await.expect("spawn join") {
        Ok(pair) => pair,
        Err(_err) => {
            let _ = storage::write_json(
                &cache_path,
                &json!({
                    "status": "failed",
                    "input_hash": input_hash,
                    "model": config.ai.model,
                    "error": "viewer context build failed",
                    "elapsed_seconds": 0.0,
                    "runtime": stats_json(&RuntimeStats::default()),
                }),
            );
            return (uid, ViewerStage::Failed);
        }
    };
    let started = std::time::Instant::now();
    let mut trace = Trace::new(trace_path);
    let mut spec = viewer_agent_spec(&uid, &rules);
    trace_run_start(
        &mut trace,
        &spec.name,
        &config.ai.model,
        "submit_viewer_perception",
        "live_core::models::ViewerPerceptionSubmission",
    );
    let prompt = viewer_user_prompt(&input_payload);
    let mut ctx = ViewerAgentCtx {
        viewer_data: context_data,
        episodes,
        research,
        store,
        slot: Default::default(),
    };
    let label = format!("观众 {name}");
    let outcome = run_toolcall_agent::<ViewerAgentCtx, ViewerPerceptionSubmission>(
        &runtime,
        &mut spec,
        AttemptPlan {
            label: &label,
            prompt: &prompt,
            max_turns: config.ai.agent.max_turns as usize,
            retries: config.ai.agent.run_retries as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
        },
        &mut ctx,
        &mut trace,
    )
    .await;
    let elapsed = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let runtime_payload = stats_json(&trace.stats);
    let ViewerAgentCtx {
        research: child, ..
    } = ctx;
    match outcome {
        Ok(outcome) => {
            let payload = serde_json::to_value(&outcome.submission).expect("submission 序列化");
            let _ = storage::write_json(
                &cache_path,
                &json!({
                    "status": "complete",
                    "input_hash": input_hash,
                    "model": config.ai.model,
                    "protocol": "terminal_tool_call",
                    "terminal_tool": "submit_viewer_perception",
                    "elapsed_seconds": elapsed,
                    "runtime": runtime_payload,
                    "analysis": payload,
                }),
            );
            (
                uid,
                ViewerStage::Ok(Box::new(ViewerTaskOut {
                    analysis: payload,
                    child,
                })),
            )
        }
        Err(err) => {
            let _ = storage::write_json(
                &cache_path,
                &json!({
                    "status": "failed",
                    "input_hash": input_hash,
                    "model": config.ai.model,
                    "error": err.to_string(),
                    "elapsed_seconds": elapsed,
                    "runtime": runtime_payload,
                }),
            );
            (uid, ViewerStage::Failed)
        }
    }
}

/// 整体态势阶段（Python `_run_audience`）：输入 → hash → 缓存恢复 → agent → 落盘。
/// research 按值进/出（含 blocking client——处置集中在 run_pipeline_inner 的 spawn_blocking）。
#[allow(clippy::too_many_arguments)]
async fn run_audience_stage(
    analysis: &Value,
    viewer_map: &Map<String, Value>,
    graph_context: &Value,
    research: ResearchService,
    config: &Config,
    runtime: &AgentRuntime,
    knobs: &PipelineKnobs<'_>,
    force: bool,
) -> (Result<(Value, Value), PipelineError>, ResearchService) {
    let input = match build_audience_input(analysis, viewer_map, graph_context) {
        Ok(input) => input,
        Err(err) => {
            return (Err(PipelineError::Message(err.to_string())), research);
        }
    };
    let input_hash = stable_hash(&json!({
        "runtime": CACHE_RUNTIME_AUDIENCE,
        "model": config.ai.model,
        "api": config.ai.api,
        "reasoning": reasoning_json(config),
        "rules": config.ai.rules,
        "input": input,
    }));
    let cache_path = config.output_dir.join("ai").join("situation.json");
    if !force
        && config.ai.agent.resume
        && let Some(cached) = storage::read_json(&cache_path).ok().flatten()
        && complete_cache(&cached, &input_hash)
        && let Ok(submission) =
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
            return (Ok((cached["analysis"].clone(), runtime)), research);
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
    let mut ctx = AudienceAgentCtx {
        viewer_analyses: viewer_map.clone(),
        research,
        store,
        graph_run_id: None,
        slot: Default::default(),
    };
    let prompt = audience_user_prompt(&input);
    let started = std::time::Instant::now();
    let outcome = run_toolcall_agent::<AudienceAgentCtx, AudienceSituationSubmission>(
        runtime,
        &mut spec,
        AttemptPlan {
            label: "整体Situation",
            prompt: &prompt,
            max_turns: config.ai.agent.max_turns as usize,
            retries: config.ai.agent.run_retries as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
        },
        &mut ctx,
        &mut trace,
    )
    .await;
    let elapsed = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let runtime_payload = stats_json(&trace.stats);
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
                    "analysis": overall,
                }),
            )
            .map_err(PipelineError::Storage);
            (write.map(|()| (overall, runtime_payload)), research)
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

fn utc_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// Python except BaseException 段：fail_run(aborted=false) + state failed。
fn fail_run_and_state(
    config: &Config,
    state_path: &Path,
    viewer_input_hashes: &Map<String, Value>,
    store: &Store,
    run_id: &str,
    error: &dyn std::fmt::Display,
    viewer_stage_complete: bool,
) {
    let _ = store.fail_run(run_id, &error.to_string(), false);
    let _ = write_state(
        state_path,
        json!({
            "status": "failed",
            "viewer_stage_status": if viewer_stage_complete { "complete" } else { "incomplete" },
            "updated_at": utc_now(),
            "model": config.ai.model,
            "error": error.to_string(),
            "viewer_input_hashes": viewer_input_hashes,
            "graph_run_id": run_id,
        }),
    );
}

/// 主体（Python `run_async`）。ctrl-c → aborted=true 收尾（D-3）。
pub async fn run_pipeline(
    config: Config,
    analysis: &Value,
    force: bool,
    knobs: &mut PipelineKnobs<'_>,
) -> Result<Value, PipelineError> {
    let runtime = Arc::new(
        AgentRuntime::from_ai_config(&config.ai)
            .map_err(|err| PipelineError::Message(err.to_string()))?,
    );
    let inner = run_pipeline_inner(&config, analysis, force, knobs, &runtime);
    tokio::select! {
        result = inner => result,
        _ = tokio::signal::ctrl_c() => {
            let state_path = config.output_dir.join("ai").join("state.json");
            if let Ok(Some(existing)) = storage::read_json(&state_path)
                && let Some(run_id) = existing.get("graph_run_id").and_then(Value::as_str)
            {
                if let Ok(store) = Store::open(&graph_file(&config.output_dir)) {
                    let _ = store.fail_run(run_id, "KeyboardInterrupt", true);
                }
                let _ = write_state(
                    &state_path,
                    json!({
                        "status": "interrupted",
                        "updated_at": utc_now(),
                        "model": config.ai.model,
                        "error": "KeyboardInterrupt",
                        "graph_run_id": run_id,
                    }),
                );
            }
            Err(PipelineError::Message("KeyboardInterrupt".to_string()))
        }
    }
}

async fn run_pipeline_inner(
    config: &Config,
    analysis: &Value,
    force: bool,
    knobs: &mut PipelineKnobs<'_>,
    runtime: &Arc<AgentRuntime>,
) -> Result<Value, PipelineError> {
    let root = config.output_dir.clone();
    let ai_root = root.join("ai");
    let viewer_cache_dir = ai_root.join("perception").join("viewers");
    std::fs::create_dir_all(&viewer_cache_dir).map_err(|err| err.to_string())?;
    if force {
        if let Ok(entries) = std::fs::read_dir(&viewer_cache_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "json") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let _ = std::fs::remove_file(ai_root.join("research_cache.json"));
    }
    let state_path = ai_root.join("state.json");
    let raw_viewers: Map<String, Value> = load_viewers(&root)
        .map_err(PipelineError::Storage)?
        .into_iter()
        .filter_map(|viewer| {
            let id = viewer["viewer"]["id"].as_str()?.to_string();
            Some((id, viewer))
        })
        .collect();
    let baseline_profiles: Vec<Value> = analysis
        .get("viewer_profiles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let viewer_ids: Vec<String> = baseline_profiles
        .iter()
        .filter_map(|profile| profile["viewer"]["id"].as_str().map(str::to_string))
        .filter(|uid| raw_viewers.contains_key(uid))
        .collect();
    let reasoning = reasoning_json(config);
    let mut bundles: HashMap<String, ViewerInputBundle> = HashMap::new();
    for uid in &viewer_ids {
        let profile = baseline_profiles
            .iter()
            .find(|p| p["viewer"]["id"].as_str() == Some(uid.as_str()))
            .cloned()
            .unwrap_or(json!({}));
        bundles.insert(
            uid.clone(),
            viewer_input_bundle(
                &raw_viewers[uid],
                &profile,
                &config.ai.model,
                &config.ai.api,
                &reasoning,
                &config.ai.rules,
                config.perception.max_evidence_per_viewer as usize,
            ),
        );
    }
    let viewer_input_hashes: Map<String, Value> = bundles
        .iter()
        .map(|(uid, bundle)| (uid.clone(), json!(bundle.input_hash.clone())))
        .collect();
    let run_started_at = utc_now();
    let graph_file = graph_file(&root);
    let store = Store::open(&graph_file)?;
    let run_id = store.begin_run(&config.ai.model)?;
    progress_say(knobs, "[GRAPH] 写入Episode、Mention、Entity和兴趣状态");
    write_state(
        &state_path,
        json!({
            "status": "running",
            "started_at": run_started_at,
            "model": config.ai.model,
            "protocol": "tool_call_only",
            "viewer_input_hashes": viewer_input_hashes,
            "graph_run_id": run_id,
        }),
    )?;
    // master research（扇出前构造；absorb 归宿；含 blocking client → 全程 spawn_blocking 处置）。
    let mut master = {
        let (root_c, cfg, origin) = (root.clone(), config.clone(), knobs.bilibili_origin.clone());
        tokio::task::spawn_blocking(move || {
            let client = new_client(&cfg, origin.as_ref().map(|(a, l)| (a.as_str(), l.as_str())))
                .map_err(|err| err.to_string())?;
            Ok::<ResearchService, String>(ResearchService::new(
                &root_c,
                client,
                cfg.ai.search_results_per_query,
            ))
        })
        .await
        .expect("spawn join")
        .map_err(PipelineError::Message)?
    };
    // 并发扇出 + 有序应用栅栏
    let semaphore = Arc::new(Semaphore::new(INVESTIGATE_CONCURRENCY));
    let mut set: tokio::task::JoinSet<(String, ViewerStage)> = tokio::task::JoinSet::new();
    for uid in &viewer_ids {
        let profile = baseline_profiles
            .iter()
            .find(|p| p["viewer"]["id"].as_str() == Some(uid.as_str()))
            .cloned()
            .unwrap_or(json!({}));
        let name = profile["viewer"]["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .unwrap_or(uid)
            .to_string();
        progress_say(knobs, &format!("[AI] Grounded Perception {name}（{uid}）"));
        let bundle = bundles.remove(uid).expect("bundle per uid");
        let cache_path = viewer_cache_dir.join(format!("{uid}.json"));
        let trace_path = config
            .ai
            .agent
            .local_trace
            .then(|| ai_root.join("traces").join(format!("viewer-{uid}.jsonl")));
        let (sem, cfg_arc, rt, gf, origin) = (
            semaphore.clone(),
            Arc::new(config.clone()),
            runtime.clone(),
            graph_file.clone(),
            knobs.bilibili_origin.clone(),
        );
        let uid_task = uid.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            run_one_viewer(
                uid_task, name, bundle, cache_path, trace_path, gf, cfg_arc, rt, origin, force,
            )
            .await
        });
    }
    let mut outputs: HashMap<String, ViewerStage> = HashMap::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((uid, stage)) => {
                outputs.insert(uid, stage);
            }
            Err(err) => {
                // 任务 panic/取消：Python gather 吞异常为 None 的镜像（该观众缺席）。
                progress_say(knobs, &format!("[AI] 观众任务崩溃：{err}"));
            }
        }
    }
    // 栅栏：viewer_ids 序应用 + absorb；失败 → graph_failures 明细 + 缓存 graph_failed。
    let mut viewer_submissions: Map<String, Value> = Map::new();
    let mut graph_failures: Vec<Value> = Vec::new();
    for uid in &viewer_ids {
        let Some(stage) = outputs.remove(uid) else {
            continue;
        };
        let analysis = match stage {
            ViewerStage::Ok(out) => {
                master.absorb_from(&out.child);
                let child = out.child;
                tokio::task::spawn_blocking(move || drop(child))
                    .await
                    .expect("spawn join");
                out.analysis
            }
            ViewerStage::Reused(analysis) => analysis,
            ViewerStage::Failed => continue,
        };
        let profile = baseline_profiles
            .iter()
            .find(|p| p["viewer"]["id"].as_str() == Some(uid.as_str()))
            .cloned()
            .unwrap_or(json!({}));
        let name = profile["viewer"]["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .unwrap_or(uid)
            .to_string();
        // Python 持 viewer_inputs 常驻；此处按 uid 等价重建 bundle（确定性纯函数）。
        let rebuilt = viewer_input_bundle(
            &raw_viewers[uid],
            &profile,
            &config.ai.model,
            &config.ai.api,
            &reasoning,
            &config.ai.rules,
            config.perception.max_evidence_per_viewer as usize,
        );
        let Some(submission) =
            serde_json::from_value::<ViewerPerceptionSubmission>(analysis.clone()).ok()
        else {
            viewer_submissions.insert(uid.clone(), analysis);
            continue;
        };
        let applied = match knobs.apply_viewer.as_deref_mut() {
            Some(hook) => hook(&store, &run_id, &name, &rebuilt.episodes, &submission),
            None => build::apply_viewer_submission(
                &store,
                &run_id,
                &name,
                &rebuilt.episodes,
                &submission,
            ),
        };
        if let Err(err) = applied {
            graph_failures.push(json!({
                "viewer_id": uid,
                "stage": "graph",
                "error": err.to_string(),
            }));
            let cache_path = viewer_cache_dir.join(format!("{uid}.json"));
            if let Ok(Some(mut cached)) = storage::read_json(&cache_path)
                && cached.is_object()
            {
                cached["status"] = json!("graph_failed");
                cached["error"] = json!(err.to_string());
                let _ = storage::write_json(&cache_path, &cached);
            }
            progress_say(knobs, &format!("[GRAPH] 观众 {uid} 写入失败：{err}"));
            continue;
        }
        viewer_submissions.insert(uid.clone(), analysis);
        if let Some(checkpoint) = knobs.checkpoint.as_deref_mut() {
            checkpoint();
        }
    }
    if viewer_submissions.is_empty() {
        fail_run_and_state(
            config,
            &state_path,
            &viewer_input_hashes,
            &store,
            &run_id,
            &"all viewer Perception or graph applies failed",
            false,
        );
        tokio::task::spawn_blocking(move || drop(master))
            .await
            .expect("spawn join");
        return Err(PipelineError::Message(
            "all viewer Perception or graph applies failed".to_string(),
        ));
    }
    let graph_context = project(
        &store,
        &ProjectOptions {
            include_episodes: false,
            include_interest_states: true,
            include_situation_actions: false,
            current_run_id: Some(run_id.clone()),
            limit: Some(AUDIENCE_GRAPH_LIMIT),
            minimum_community_size: config.perception.minimum_community_size,
            ..ProjectOptions::default()
        },
    )?;
    let viewer_runtime: Vec<Value> = viewer_submissions
        .keys()
        .filter_map(|uid| {
            storage::read_json(&viewer_cache_dir.join(format!("{uid}.json")))
                .ok()
                .flatten()
                .filter(|v| v.is_object())
                .and_then(|cached| cached.get("runtime").cloned())
        })
        .collect();
    let viewer_count = viewer_submissions.len() as i64;
    write_state(
        &state_path,
        json!({
            "status": "viewer_complete",
            "started_at": run_started_at,
            "viewer_stage_completed_at": utc_now(),
            "model": config.ai.model,
            "protocol": "tool_call_only",
            "viewer_count": viewer_count,
            "viewer_failures": graph_failures,
            "viewer_input_hashes": viewer_input_hashes,
            "graph_run_id": run_id,
        }),
    )?;
    let (audience, master) = run_audience_stage(
        analysis,
        &viewer_submissions,
        &graph_context,
        master,
        config,
        runtime,
        knobs,
        force,
    )
    .await;
    let (overall, overall_runtime) = match audience {
        Ok(pair) => pair,
        Err(err) => {
            fail_run_and_state(
                config,
                &state_path,
                &viewer_input_hashes,
                &store,
                &run_id,
                &err,
                true,
            );
            tokio::task::spawn_blocking(move || drop(master))
                .await
                .expect("spawn join");
            return Err(err);
        }
    };
    // 回读 situation.json 的 input_hash（Python：丢了即 AgentRuntimeError）。
    let situation_input_hash = storage::read_json(&ai_root.join("situation.json"))
        .ok()
        .flatten()
        .filter(|v| v.is_object())
        .and_then(|cached| {
            cached
                .get("input_hash")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    if situation_input_hash.is_empty() {
        let err =
            PipelineError::Message("completed Situation is missing its input hash".to_string());
        fail_run_and_state(
            config,
            &state_path,
            &viewer_input_hashes,
            &store,
            &run_id,
            &err,
            true,
        );
        tokio::task::spawn_blocking(move || drop(master))
            .await
            .expect("spawn join");
        return Err(err);
    }
    if let Some(audience_submission) =
        serde_json::from_value::<AudienceSituationSubmission>(overall.clone()).ok()
        && let Err(err) = build::apply_audience_submission(&store, &run_id, &audience_submission)
    {
        let wrapped = PipelineError::Store(err);
        fail_run_and_state(
            config,
            &state_path,
            &viewer_input_hashes,
            &store,
            &run_id,
            &wrapped,
            true,
        );
        tokio::task::spawn_blocking(move || drop(master))
            .await
            .expect("spawn join");
        return Err(wrapped);
    }
    store.complete_run(&run_id)?;
    let usage = aggregate_runtime_usage(&viewer_runtime, &overall_runtime);
    write_state(
        &state_path,
        json!({
            "status": "complete",
            "completed_at": utc_now(),
            "model": config.ai.model,
            "protocol": "tool_call_only",
            "situation_input_hash": situation_input_hash,
            "viewer_input_hashes": viewer_input_hashes,
            "graph_run_id": run_id,
            // D-1（design-Δ）：token 成本一等公民入 state.json。
            "usage": usage,
        }),
    )?;
    let final_result = json!({
        "status": "complete",
        "runtime": "openai-agents",
        "viewer_count": viewer_count,
        "viewer_failures": (viewer_ids.len() as i64) - viewer_count,
        "concrete_interest_count": overall["interest_graph"].as_array().map_or(0, Vec::len) as i64,
        "content_opportunity_count": overall["content_opportunities"]
            .as_array()
            .map_or(0, Vec::len) as i64,
        "search_result_count": master.search_results.len() as i64,
        "graph": {"database": graph_file.display().to_string()},
        "usage": usage,
    });
    tokio::task::spawn_blocking(move || drop(master))
        .await
        .expect("spawn join");
    Ok(final_result)
}
