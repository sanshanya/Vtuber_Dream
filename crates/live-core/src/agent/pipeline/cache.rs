//! 认知缓存键族（M4-A 输入小件）。
//!
//! golden 对账：tests/pipeline_inputs.rs × tests-fixtures/m4a/（Python 实算，
//! 由 mod 壳再导出保持 `live_core::agent::pipeline::*` 坐标不漂移）。
//! 语义哈希口径：`observed_at` 等过程/墙钟指标只从哈希件摘除，LLM 提示面原样
//! 保真——事实相同的两次采集必须产出同一 input_hash，「重采保 AI」才成立。

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::episodes::{self, Episode, baseline::viewer_context};
use crate::storage::{self, load_viewers};

/// 缓存键 runtime 串（Python `_viewer_input_bundle` 逐字；跨实现缓存可比的锚，不得改字）。
pub const CACHE_RUNTIME_VIEWER: &str = "openai-agents-toolcall-grounded-v0.12-validated-cache";
/// 缓存键 runtime 串（Python `_run_audience` 逐字）。
pub const CACHE_RUNTIME_AUDIENCE: &str = "openai-agents-toolcall-situation-v0.12-validated-cache";

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

/// episode 集合身份 —— blake2s256(canonical_json 化的排序后 episode_id 列表)。
/// episode_id 已焊 content_version 16 位内容摘要（episodes/build.rs），本函数只对
/// 「集合成员资格」的身份二次定海：成员重排不翻面（序无关），成员增删/内容变才翻面。
/// 域分离：kind 字面防与 stable_hash 家族值域互撞。（圆桌性能席 blake2b 提案的收敛实现：
/// set 输入为已摘要化 id 串、体量数百字节，blake2s256 全输出即足，不引 digest_size 魔数。）
pub fn episode_set_hash(episodes: &[Episode]) -> String {
    let mut ids: Vec<&str> = episodes.iter().map(|e| e.episode_id.as_str()).collect();
    ids.sort_unstable();
    format!(
        "{:x}",
        blake2::Blake2s256::digest(
            canonical_json(&json!({"kind": "episode-set-v1", "ids": ids})).as_bytes()
        )
    )
}

// ---------------------------------------------------------------------------
// viewer_input_bundle（Python pipeline._viewer_input_bundle）
// ---------------------------------------------------------------------------

pub struct ViewerInputBundle {
    pub context_data: Value,
    pub episodes: Vec<Episode>,
    pub input_payload: Value,
    pub input_hash: String,
    /// 壳身份：config 描述子 + 去 episodes 的输入包。episode 面任意漂移
    /// （增删/内容变/重排）都不翻面——「环境没变」与「证据变了」从此可分账。
    pub shell_hash: String,
    /// 集合身份：episode_id 焊 content_version，排序无关（episode_set_hash）。
    pub episode_set_hash: String,
}

/// 语义哈希口径：`observed_at` = 观察时刻（本轮采集墙钟），不是事实内容。
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
/// hash 件用 episodes_hash_material（摘 observed_at），payload 原样保真。
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
    // input_hash 保持与 Python golden 逐字对账的整包口径；壳/集合同源新算。
    // 壳 = 环境+描述子：episodes 与其导出物（deterministic_mention_seeds 由 episodes
    // 函数派生）同属证据层，两面同时摘出——否则证据变 = 壳变，双轨语义塌陷。
    let mut shell_input = input_payload
        .as_object()
        .cloned()
        .expect("payload 恒为对象");
    shell_input.remove("episodes");
    shell_input.remove("deterministic_mention_seeds");
    let shell_hash = stable_hash(&json!({
        "runtime": CACHE_RUNTIME_VIEWER,
        "model": model,
        "api": api,
        "reasoning": reasoning,
        "rules": rules,
        "input": shell_input,
    }));
    let episode_set_hash = episode_set_hash(&episodes);
    ViewerInputBundle {
        context_data,
        episodes,
        input_payload,
        input_hash,
        shell_hash,
        episode_set_hash,
    }
}

// ---------------------------------------------------------------------------
// 缓存判定与名册预估
// ---------------------------------------------------------------------------

/// reasoning 配置投影（输入哈希成分之一，cache/audience 两面共用的同源件）。
pub(crate) fn reasoning_json(config: &Config) -> Value {
    json!({
        "enabled": config.ai.reasoning.enabled,
        "effort": config.ai.reasoning.effort,
        "replay_content": config.ai.reasoning.replay_content,
    })
}

/// `_complete_cache`：dict ∧ status=="complete" ∧ hash 相等 ∧ analysis 是 dict。
pub(crate) fn complete_cache(cache: &Value, input_hash: &str) -> bool {
    cache.is_object()
        && cache.get("status").and_then(Value::as_str) == Some("complete")
        && cache.get("input_hash").and_then(Value::as_str) == Some(input_hash)
        && cache.get("analysis").is_some_and(Value::is_object)
}

/// 名册 →「本轮真的会新建/更新」的 fresh 集合（预算闸的分子，唯一口径件）。
/// fresh = 输入哈希已变 ∪ 无完整旧结论（含缓存缺失/读坏一律算新鲜——估错方向
/// 只会保守多估，绝不漏估烧成实账）。与执行面同源：扇出后 run_one_viewer 的
/// complete_cache 短路恰是这个集合的补集，预估从此不再与实跑两张皮。
pub fn fresh_viewer_ids(
    viewer_ids: &[String],
    bundles: &HashMap<String, ViewerInputBundle>,
    viewer_cache_dir: &Path,
) -> Vec<String> {
    viewer_ids
        .iter()
        .filter(|uid| {
            let Some(bundle) = bundles.get(*uid) else {
                return true;
            };
            let cached = storage::read_json(&viewer_cache_dir.join(format!("{uid}.json")))
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            !complete_cache(&cached, &bundle.input_hash)
        })
        .cloned()
        .collect()
}

/// 薄 GET（/api/budget）预估口径：与运行内预算闸同公式同源 fresh。
/// 返回 (fresh, total)；名册/baseline 缺席 → None（前端「预估 —」不臆造）。
pub fn roster_estimate(config: &Config) -> Option<(usize, usize)> {
    let analysis = episodes::baseline::build_factual_baseline(
        &config.output_dir,
        config.perception.max_evidence_per_viewer as usize,
    )
    .ok()?;
    let profiles = analysis.get("viewer_profiles")?.as_array()?;
    if profiles.is_empty() {
        return None;
    }
    let raw_viewers: Map<String, Value> = load_viewers(&config.output_dir)
        .ok()?
        .into_iter()
        .filter_map(|viewer| {
            let id = viewer["viewer"]["id"].as_str()?.to_string();
            Some((id, viewer))
        })
        .collect();
    let viewer_ids: Vec<String> = profiles
        .iter()
        .filter_map(|profile| profile["viewer"]["id"].as_str().map(str::to_string))
        .filter(|uid| raw_viewers.contains_key(uid))
        .collect();
    if viewer_ids.is_empty() {
        return None;
    }
    let reasoning = reasoning_json(config);
    let mut bundles: HashMap<String, ViewerInputBundle> = HashMap::new();
    for uid in &viewer_ids {
        let profile = profiles
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
    let viewer_cache_dir = config
        .output_dir
        .join("ai")
        .join("perception")
        .join("viewers");
    let fresh = fresh_viewer_ids(&viewer_ids, &bundles, &viewer_cache_dir);
    Some((fresh.len(), viewer_ids.len()))
}

/// 舰长感知缓存的「时效位」（用户裁决：旧 AI 结论保留作参考、不删——但
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
