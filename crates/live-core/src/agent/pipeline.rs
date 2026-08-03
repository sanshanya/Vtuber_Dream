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
