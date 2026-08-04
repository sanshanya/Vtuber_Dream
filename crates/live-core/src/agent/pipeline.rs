//! AudienceAnalysisAgent 编排（移植 `agent/pipeline.py`；Peer 链挂 G2）。
//!
//! 体积备书（r8-F2）：1439 行超 800 行拆分线——但本卷是 Python parity 逐段对照面，
//! golden 对账按段定位（M4-A..M4-D 段标题与 tests/fixtures 同名），拆卷会打断
//! 「Python 旧实现即预言机」的坐标系。G2/后议再拆，拆分锚 = Python 顶层函数边界。
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

/// Z5 语义哈希口径：`observed_at` = 观察时刻（本轮采集墙钟），不是事实内容。
/// 事实相同的两次采集必须产出同一 input_hash——否则 complete_cache 跨采集恒假死，
/// 「重采保 AI」成空架（reset_output 全删 ai/ 时代这个问题被物理掩盖）。
/// 只从哈希件中摘除；LLM 提示面（input_payload.episodes）继续保留
/// observed_at——时间域对推理仍是事实，账本上只摘口径，不摘信息。
fn episodes_hash_material(episodes: &[Episode]) -> Vec<Value> {
    episodes
        .iter()
        .map(|episode| {
            let mut value = serde_json::to_value(episode).expect("Episode 恒可序列化");
            if let Value::Object(map) = &mut value {
                map.remove("observed_at");
            }
            value
        })
        .collect()
}

/// Python `_viewer_input_bundle`：context + episodes + payload + hash 四元组。
/// `reasoning`/`rules`/`model`/`api` 只参与 hash，不改写平台事实。
/// Z5：hash 件用 episodes_hash_material（摘 observed_at），payload 原样保真。
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
    let mut hash_input = input_payload
        .as_object()
        .cloned()
        .expect("payload 恒为对象");
    hash_input.insert(
        "episodes".to_string(),
        json!(episodes_hash_material(&episodes)),
    );
    let input_hash = stable_hash(&json!({
        "runtime": CACHE_RUNTIME_VIEWER,
        "model": model,
        "api": api,
        "reasoning": reasoning,
        "rules": rules,
        "input": hash_input,
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
/// Z5 语义哈希口径（audience 侧，与 episodes_hash_material 同族）：baseline_summary 的
/// collection_request_count / collection_elapsed_seconds 是**过程指标**（每轮采集必变），
/// 不是听众事实——计入哈希会让 situation 缓存跨采集恒假死。提示面原样保留（与 Python
/// 预言机 golden 对账面不漂移），哈希件剔除这两键。
fn audience_input_hash_material(input: &Value) -> Value {
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
use crate::leads;
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
    /// r2-F8：通道可失败——Err 不穿透（Python 吞异常记 progress 同型）。
    pub checkpoint: Option<&'a mut dyn FnMut() -> Result<(), String>>,
    /// 测试接缝：graph_failed 分支的确定性复现（默认 build::apply_viewer_submission）。
    pub apply_viewer: Option<&'a mut ApplyViewerFn<'a>>,
    /// 测试接缝：Bilibili 回放根地址（默认官方端点；调查工具发起网络时才消费）。
    pub bilibili_origin: Option<(String, String)>,
    /// M5-B3/D3：run 状态机 stage hook。已公布的字面名只来自本文件两个常量；
    /// queued/collecting/episodes/done/failed 由调用方（live-server registry）控制——
    /// 显式 seam 使 progress 文案与状态机解耦（不改既有测试签名：Option<'_'> 字段）。
    pub stage: Option<&'a dyn Fn(&'static str)>,
    /// Z4b（动作平面 kind=ai_viewers）：viewer 阶段写盘完成即收——不跑 audience。
    /// 语义边界：situation.json 保持上一轮；终盘 state.json 落 `stage_terminal=per_viewer_ai`。
    pub stop_after_viewer_stage: bool,
}

fn progress_say(knobs: &PipelineKnobs<'_>, message: &str) {
    if let Some(progress) = knobs.progress {
        progress(message);
    }
}

fn stage_say(knobs: &PipelineKnobs<'_>, stage: &'static str) {
    if let Some(hook) = knobs.stage {
        hook(stage);
    }
}

/// M5-B3 状态机字面名（design §10 枚举——只有 hook 内产物 + live-server registry 设出的
/// 名字声称这一组；listener 消费方对照匹配）。
pub const STAGE_PER_VIEWER_AI: &str = "per_viewer_ai";
pub const STAGE_AUDIENCE: &str = "audience";

/// r3-F3：analysis 落盘剥「空 leads」——Python 模型 extra=forbid：键存在即拒；
/// 空期双通（Rust serde 有 default 补齐、Python 无解码阻力）；非空 leads 是 M4.x
/// 新能力，跨实现缓存复用本就有界（登记 design-Δ）。
fn strip_empty_leads(mut analysis: Value) -> Value {
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
    analysis
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

/// M4.x kickoff D4 → G2 表形态（design §9.2 行 254）：leads 账本摘要注入用户消息
///（hash 之外——账本漂移不是 Python parity 的输入身份，缓存有效性不因此失效；
/// 登记 design-Δ）。数据源 = discovery_leads 表；账本为空则零面世（首跑提示面
/// 与 M4 逐字节一致）。读库失败 → 吞纳零面世（annex 是上下文增益面，不绊管线）。
fn ledger_annex(store: &Store, viewer: Option<&str>, prompt: String) -> String {
    let rows = leads::read_rows(store).unwrap_or_default();
    if rows.is_empty() {
        return prompt;
    }
    format!("{prompt}\n\n{}", leads::summary_line(&rows, viewer))
}

fn reasoning_json(config: &Config) -> Value {
    json!({
        "enabled": config.ai.reasoning.enabled,
        "effort": config.ai.reasoning.effort,
        "replay_content": config.ai.reasoning.replay_content,
    })
}

/// Z5c：舰长感知缓存的「时效位」（用户裁决：旧 AI 结论保留作参考、不删——但
/// 事实面/提示面变化后，旧结论行必须亮「信源已更新·待重判」，不得摆绿）。
/// 语义 = 「今天重跑会不会复用这条旧结论」的同源对照：
/// `complete_cache` 用的完整判定里只有哈希对决策有话语权，此处频道专属：
/// - Some(true)：旧 complete 结论存在，但当前输入哈希已变 → 旧信源，行面提亮示
/// - Some(false)：旧 complete 结论存在且哈希相等 → 时效位亮绿灯
/// - None：无缓存 / 非 complete / 缓存缺哈希键（本就不是「可参考的旧结论」，
///   交给 ai_completed 行面自证——不是该亮标的语义）
pub fn viewer_perception_stale(
    config: &Config,
    raw_viewer: &Value,
    cached: &Value,
) -> Option<bool> {
    if cached.get("status").and_then(Value::as_str) != Some("complete") {
        return None;
    }
    let cached_hash = cached.get("input_hash").and_then(Value::as_str)?;
    let profile = crate::episodes::baseline::viewer_input(
        raw_viewer,
        config.perception.max_evidence_per_viewer as usize,
    );
    let bundle = viewer_input_bundle(
        raw_viewer,
        &profile,
        &config.ai.model,
        &config.ai.api,
        &reasoning_json(config),
        &config.ai.rules,
        config.perception.max_evidence_per_viewer as usize,
    );
    Some(bundle.input_hash != cached_hash)
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
        let revalidated = {
            let graph_file_ref = graph_file.clone();
            let uid_ref = uid.clone();
            let episodes_ref = episodes.clone();
            let config_ref = config.clone();
            let origin_ref = origin.clone();
            tokio::task::spawn_blocking(move || {
                let store = Store::open(&graph_file_ref).map_err(|err| err.to_string())?;
                let entity_exists =
                    |candidate: &str| store.entity_exists(candidate).unwrap_or(false);
                // r2-F3/r3-F2（评审双坐实）：Python 传共享 research 的注册表，其 ctor 从
                // research_cache.json 回填——闭包必须是「子实例磁盘视图」（含上运行归档 +
                // 本运行已落盘发现），不是空集。
                let client = new_client(
                    &config_ref,
                    origin_ref.as_ref().map(|(a, l)| (a.as_str(), l.as_str())),
                )
                .map_err(|err| err.to_string())?;
                let research = ResearchService::new(
                    &config_ref.output_dir,
                    client,
                    config_ref.ai.search_results_per_query,
                )
                .with_persistence(false);
                let search_ids: std::collections::HashSet<String> =
                    research.search_results.keys().cloned().collect();
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
        if let Ok(errors) = revalidated
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
    let prompt = ledger_annex(&store, Some(&uid), viewer_user_prompt(&input_payload));
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
            // r1-F1：单 viewer token 预算熔断（累计 total_tokens 超限 → viewer_failure）。
            token_budget: Some(config.ai.agent.viewer_token_budget),
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
                    // r3-F3：空 leads 剥键（Python extra=forbid——键存在即拒）。
                    "analysis": strip_empty_leads(payload.clone()),
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
    // Z5：哈希件走 audience_input_hash_material（过程指标摘出——同数据重采同哈希），
    // 提示面 input 原样保真（golden 对账不漂移）。
    let input_hash = stable_hash(&json!({
        "runtime": CACHE_RUNTIME_AUDIENCE,
        "model": config.ai.model,
        "api": config.ai.api,
        "reasoning": reasoning_json(config),
        "rules": config.ai.rules,
        "input": audience_input_hash_material(&input),
    }));
    let cache_path = config.output_dir.join("ai").join("situation.json");
    if !force && config.ai.agent.resume {
        let cached = storage::read_json(&cache_path).ok().flatten();
        if let Some(cached) = cached.as_ref() {
            if !complete_cache(cached, &input_hash) {
                // Z5 观测面：situation 缓存命中判别不可静默——哈希不等 / 复核不过 / 形态坏
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
                    return (Ok((cached["analysis"].clone(), runtime)), research);
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
            max_turns: config.ai.agent.max_turns as usize,
            retries: config.ai.agent.run_retries as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
            // r1-F1 熔断只属 viewer 面；audience 单 agent 不设 budget。
            token_budget: None,
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
                    "analysis": strip_empty_leads(overall.clone()),
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

/// Python except BaseException 段（兜底伞）：fail_run + failed/interrupted 七键 state。
/// run_id=None 时跳过 fail_run（Python：graph_repo=None 不落 fail 行，state 照写）。
fn fail_run_and_state(
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
        // 唯一 true 的来源是 ctrl-c（KeyboardInterrupt 同型物，D-3）。
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
struct UmbrellaNote {
    run_id: Option<String>,
    viewer_stage_complete: bool,
    viewer_input_hashes: Map<String, Value>,
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
    // Python 次序：research(master) 在 begin_graph_run 之前构造；其失败走裸传播（不 umbrella）。
    // 含 blocking client → 构造与处置都走 spawn_blocking（D-2）。
    let master = {
        let (root, cfg, origin) = (
            config.output_dir.clone(),
            config.clone(),
            knobs.bilibili_origin.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let client = new_client(&cfg, origin.as_ref().map(|(a, l)| (a.as_str(), l.as_str())))
                .map_err(|err| err.to_string())?;
            Ok::<ResearchService, String>(ResearchService::new(
                &root,
                client,
                cfg.ai.search_results_per_query,
            ))
        })
        .await
        .expect("spawn join")
        .map_err(PipelineError::Message)?
    };
    let inner = run_pipeline_inner(&config, analysis, force, knobs, &runtime, master);
    tokio::select! {
        (result, master) = inner => {
            tokio::task::spawn_blocking(move || drop(master))
                .await
                .expect("spawn join");
            result
        }
        _ = tokio::signal::ctrl_c() => {
            // M-D（评审）：interrupted 也写满七键；error = Python str(KeyboardInterrupt()) → 空串。
            let state_path = config.output_dir.join("ai").join("state.json");
            let existing = storage::read_json(&state_path).ok().flatten();
            let viewer_stage_complete =
                existing.as_ref().and_then(|s| s.get("status")).and_then(Value::as_str)
                    == Some("viewer_complete");
            let viewer_input_hashes = existing
                .as_ref()
                .and_then(|s| s.get("viewer_input_hashes"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let run_id = existing
                .as_ref()
                .and_then(|s| s.get("graph_run_id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            fail_run_and_state(
                &config,
                &state_path,
                &viewer_input_hashes,
                run_id.as_deref(),
                "",
                viewer_stage_complete,
                true,
            );
            // 注意：被丢弃的 inner future 内含 master（blocking client）——M3 钉死的红线
            // 是「构造」而非「drop」，此处非显式 spawn_blocking 处置属已知边界。
            Err(PipelineError::Message("KeyboardInterrupt".to_string()))
        }
    }
}

/// D7（M5-B3）：单观众薄封装——analysis 的 viewer_profiles 过滤后走同一 run_pipeline
/// 主体；viewer 未落入 baseline → 明确错误，不新写编排（kickoff G1/D7 裁决）。
///
/// 注：run_pipeline 的 force 语义为**全局**清理（重算所有篮内观众），viewer 面 force 的
/// 拒绝职责归调用方（live-server POST /api/runs 的 422）。
pub async fn run_viewer_pipeline(
    config: Config,
    analysis: &Value,
    viewer_uid: &str,
    force: bool,
    knobs: &mut PipelineKnobs<'_>,
) -> Result<Value, PipelineError> {
    let mut filtered = analysis.clone();
    if let Some(profiles) = filtered
        .get_mut("viewer_profiles")
        .and_then(Value::as_array_mut)
    {
        profiles.retain(|profile| profile["viewer"]["id"].as_str() == Some(viewer_uid));
    }
    match filtered["viewer_profiles"].as_array() {
        Some(list) if !list.is_empty() => {}
        _ => {
            return Err(PipelineError::Message(format!(
                "baseline 无 viewer {viewer_uid}"
            )));
        }
    }
    run_pipeline(config, &filtered, force, knobs).await
}

async fn run_pipeline_inner(
    config: &Config,
    analysis: &Value,
    force: bool,
    knobs: &mut PipelineKnobs<'_>,
    runtime: &Arc<AgentRuntime>,
    master: ResearchService,
) -> (Result<Value, PipelineError>, ResearchService) {
    let root = config.output_dir.clone();
    let ai_root = root.join("ai");
    let viewer_cache_dir = ai_root.join("perception").join("viewers");
    let state_path = ai_root.join("state.json");

    // ── Python try 之前的面（prep 段裸传播：不 umbrella、不写 state）──
    if let Err(err) = std::fs::create_dir_all(&viewer_cache_dir).map_err(|err| err.to_string()) {
        return (Err(PipelineError::Message(err)), master);
    }
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
    let raw_viewers: Map<String, Value> = match load_viewers(&root).map_err(PipelineError::Storage)
    {
        Ok(result) => result
            .into_iter()
            .filter_map(|viewer| {
                let id = viewer["viewer"]["id"].as_str()?.to_string();
                Some((id, viewer))
            })
            .collect(),
        Err(err) => return (Err(err), master),
    };
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
    // ── try 段（Python except BaseException 的承保域）──
    let mut note = UmbrellaNote {
        run_id: None,
        viewer_stage_complete: false,
        viewer_input_hashes: viewer_input_hashes.clone(),
    };
    let mut master = Some(master);
    macro_rules! bail {
        ($err:expr) => {{
            let err: PipelineError = $err;
            fail_run_and_state(
                config,
                &state_path,
                &note.viewer_input_hashes,
                note.run_id.as_deref(),
                &err.to_string(),
                note.viewer_stage_complete,
                false,
            );
            return (Err(err), master.take().expect("master 归还"));
        }};
    }
    let run_started_at = utc_now();
    let graph_file = graph_file(&root);
    let store = match Store::open(&graph_file) {
        Ok(store) => store,
        Err(err) => bail!(PipelineError::Store(err)),
    };
    // G2（design §9.2 行 254）：M4.x JSONL 账本一次性入库 + 归档 .bak（幂等；
    // 守卫失败响铃不绊管线——publish 读取面以表为准，迁移下轮再试。
    if let Err(err) = leads::migrate_jsonl(&store, &config.output_dir) {
        progress_say(knobs, &format!("[LEADS] 旧 JSONL 账本入库迁移失败：{err}"));
    }
    let run_id = match store.begin_run(&config.ai.model) {
        Ok(run_id) => run_id,
        Err(err) => bail!(PipelineError::Store(err)),
    };
    note.run_id = Some(run_id.clone());
    progress_say(knobs, "[GRAPH] 写入Episode、Mention、Entity和兴趣状态");
    match write_state(
        &state_path,
        json!({
            "status": "running",
            "started_at": run_started_at,
            "model": config.ai.model,
            "protocol": "tool_call_only",
            "viewer_input_hashes": viewer_input_hashes,
            "graph_run_id": run_id,
        }),
    ) {
        Ok(()) => {}
        Err(err) => bail!(err),
    }
    // master 由 run_pipeline 传入（Python 次序：research 先于 begin_graph_run）。
    // 并发扇出 + 有序应用栅栏（M5-B3：状态机 hook——queued/collecting/episodes 由
    // registry 自己直接进入 per_viewer_ai）。
    stage_say(knobs, STAGE_PER_VIEWER_AI);
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
                master
                    .as_mut()
                    .expect("master 在场")
                    .absorb_from(&out.child);
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
        // M4.x kickoff D3：Ok/Reused 双臂在 apply 成功点汇流——账本补写幂等，
        // 缓存命中路径同样补账（首跑中断后恢复不丢账）。
        // G2 表形态（design §9.2）：dedication 表 OR IGNORE 幂等，唯一键即 dedupe_key。
        // MXA-4（r6 驳 D7「无通道」前提）：fail-open 保持但响铃——
        // 线索无痕蒸发 ≠ 豁免项。
        if let Err(err) = leads::record_leads(&store, uid, &run_id, &utc_now(), &submission.leads) {
            progress_say(knobs, &format!("[LEADS] 观众 {uid} 账本写入失败：{err}"));
        }
        if let Some(checkpoint) = knobs.checkpoint.as_deref_mut() {
            // r2-F8：报告刷新失败必须与 Python 同型——吞掉记 progress，不打断管线。
            if let Err(err) = checkpoint() {
                progress_say(knobs, &format!("[AI] 观众 {uid} checkpoint 失败：{err}"));
            }
        }
    }
    if viewer_submissions.is_empty() {
        bail!(PipelineError::Message(
            "all viewer Perception or graph applies failed".to_string(),
        ));
    }
    stage_say(knobs, STAGE_AUDIENCE);
    let graph_context = match project(
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
    ) {
        Ok(context) => context,
        Err(err) => bail!(PipelineError::Store(err)),
    };
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
    match write_state(
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
    ) {
        Ok(()) => {}
        Err(err) => bail!(err),
    }
    // viewer 阶段收口标记：兜底伞 interrupted/failed 文案的 viewer_stage_status 数据源。
    note.viewer_stage_complete = true;
    // Z4b（动作平面「舰长 AI 分析」kind）：viewer 阶段全部落盘即收——不进 audience；
    // situation.json 未动（overview.situation 保持上一轮的态势面，这些钮的语义边界）。
    if knobs.stop_after_viewer_stage {
        progress_say(
            knobs,
            "[AI] stop_after_viewer_stage：舰长分析收口（audience 未跑，整体态势未动）",
        );
        if let Err(err) = store.complete_run(&run_id) {
            bail!(PipelineError::Store(err));
        }
        let usage = aggregate_runtime_usage(&viewer_runtime, &json!({}));
        if let Err(err) = write_state(
            &state_path,
            json!({
                "status": "complete",
                "completed_at": utc_now(),
                "model": config.ai.model,
                "protocol": "tool_call_only",
                // 停点口径：AI 完成于 viewer 阶段，situation 未产出——落显式 stage_terminal。
                "stage_terminal": "per_viewer_ai",
                "viewer_input_hashes": viewer_input_hashes,
                "graph_run_id": run_id,
                "usage": usage,
            }),
        ) {
            bail!(err);
        }
        let final_result = json!({
            "status": "complete",
            "runtime": "openai-agents",
            "stage_terminal": "per_viewer_ai",
            "viewer_count": viewer_count,
            "viewer_failures": (viewer_ids.len() as i64) - viewer_count,
            "search_result_count": master
                .as_ref()
                .expect("master 归还")
                .search_results
                .len() as i64,
            "graph": {"database": graph_file.display().to_string()},
            "usage": usage,
        });
        return (Ok(final_result), master.take().expect("master 归还"));
    }
    let (audience, research) = run_audience_stage(
        analysis,
        &viewer_submissions,
        &graph_context,
        master.take().expect("master 在场"),
        config,
        runtime,
        knobs,
        force,
    )
    .await;
    master = Some(research);
    let (overall, overall_runtime) = match audience {
        Ok(pair) => pair,
        Err(err) => bail!(err),
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
        bail!(PipelineError::Message(
            "completed Situation is missing its input hash".to_string(),
        ));
    }
    if let Ok(audience_submission) =
        serde_json::from_value::<AudienceSituationSubmission>(overall.clone())
    {
        if let Err(err) = build::apply_audience_submission(&store, &run_id, &audience_submission) {
            bail!(PipelineError::Store(err));
        }
        // M4.x → G2 表形态：audience leads 以 AUDIENCE_VIEWER_ID 入账
        //（apply 成功后，同 viewer 纪律）。MXA-4：fail-open 保持但响铃。
        if let Err(err) = leads::record_leads(
            &store,
            leads::AUDIENCE_VIEWER_ID,
            &run_id,
            &utc_now(),
            &audience_submission.leads,
        ) {
            progress_say(knobs, &format!("[LEADS] 整体账本写入失败：{err}"));
        }
    }
    match store.complete_run(&run_id) {
        Ok(()) => {}
        Err(err) => bail!(PipelineError::Store(err)),
    }
    let usage = aggregate_runtime_usage(&viewer_runtime, &overall_runtime);
    match write_state(
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
    ) {
        Ok(()) => {}
        Err(err) => bail!(err),
    }
    let search_result_count = master.as_ref().expect("master 归还").search_results.len() as i64;
    let final_result = json!({
        "status": "complete",
        "runtime": "openai-agents",
        "viewer_count": viewer_count,
        "viewer_failures": (viewer_ids.len() as i64) - viewer_count,
        "concrete_interest_count": overall["interest_graph"].as_array().map_or(0, Vec::len) as i64,
        "content_opportunity_count": overall["content_opportunities"]
            .as_array()
            .map_or(0, Vec::len) as i64,
        "search_result_count": search_result_count,
        "graph": {"database": graph_file.display().to_string()},
        "usage": usage,
    });
    (Ok(final_result), master.take().expect("master 归还"))
}
