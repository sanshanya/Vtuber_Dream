//! 单观众 agent 任务：缓存恢复 → 复用臂重校验 → run_toolcall_agent → 缓存落盘。
//! 所有阻塞件（Store/ResearchService 含 blocking client）经 spawn_blocking（M3 blocker）。

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::prompts::{trace_run_start, viewer_user_prompt};
use super::super::runtime::{AgentRuntime, AttemptPlan, RuntimeStats, Trace, run_toolcall_agent};
use super::super::specs::viewer_agent_spec;
use super::super::tools::{ResearchService, ViewerAgentCtx};
use super::super::validators::validate_viewer_submission;
use super::cache::{ViewerInputBundle, complete_cache};
use super::run::{ledger_annex, new_client};
use super::state::{stats_json, strip_empty_leads};
use crate::config::Config;
use crate::episodes::Episode;
use crate::graph::store::Store;
use crate::models::ViewerPerceptionSubmission;
use crate::storage;

pub(crate) struct ViewerTaskOut {
    pub(crate) analysis: Value,
    /// 待 absorb 的子实例（含 blocking client——跨任务移动后由主任务集中处置）。
    pub(crate) child: ResearchService,
    /// 本观众本轮 LLM 的 prompt-cache 计量（hit, miss）；复用/早退为零——
    /// 语义即「本轮新发起请求的缓存命中量」（复用路径没有新请求，计数诚实归零）。
    pub(crate) cache_tally: (i64, i64),
}

pub(crate) enum ViewerStage {
    Ok(Box<ViewerTaskOut>),
    /// 缓存复用短路（Python：不再触碰 research；无新发现可吸收）。
    Reused(Value),
    /// 失败臂同样携带本轮已烧的 cache 计量（撞 budget 的浪费要入账）。
    Failed((i64, i64)),
}

/// 单观众：缓存恢复 → agent 运行 → 缓存落盘。所有阻塞件经 spawn_blocking（M3 blocker）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_one_viewer(
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
    // 壳/集合身份随缓存落盘——增量复用消费者（后续批次）的判定件。
    let hash_manifest = json!({
        "input_hash": bundle.input_hash,
        "shell_hash": bundle.shell_hash,
        "episode_set_hash": bundle.episode_set_hash,
    });
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
                // Python 传共享 research 的注册表，其 ctor 从
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
                    "hash_manifest": hash_manifest,
                    "elapsed_seconds": 0.0,
                    "runtime": stats_json(&RuntimeStats::default()),
                }),
            );
            return (uid, ViewerStage::Failed((0, 0)));
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
            // 轮数不设限（2026-08-07 官规：唯一刹车=下方 viewer_token_budget 保险丝）。
            max_turns: usize::MAX,
            retries: config.ai.agent.run_retries as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
            // 单 viewer token 预算熔断（累计 total_tokens 超限 → viewer_failure）。
            token_budget: Some(config.ai.agent.viewer_token_budget),
        },
        &mut ctx,
        &mut trace,
    )
    .await;
    let elapsed = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let runtime_payload = stats_json(&trace.stats);
    // cache 计量走独立 tally——persisted runtime 载荷守 Python 五键 parity。
    let cache_tally = (trace.stats.cache_hit_tokens, trace.stats.cache_miss_tokens);
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
                    "hash_manifest": hash_manifest,
                    "model": config.ai.model,
                    "protocol": "terminal_tool_call",
                    "terminal_tool": "submit_viewer_perception",
                    "elapsed_seconds": elapsed,
                    "runtime": runtime_payload,
                    // 空 leads 剥键（Python extra=forbid——键存在即拒）。
                    "analysis": strip_empty_leads(payload.clone()),
                }),
            );
            (
                uid,
                ViewerStage::Ok(Box::new(ViewerTaskOut {
                    analysis: payload,
                    child,
                    cache_tally,
                })),
            )
        }
        Err(err) => {
            let _ = storage::write_json(
                &cache_path,
                &json!({
                    "status": "failed",
                    "input_hash": input_hash,
                    "hash_manifest": hash_manifest,
                    "model": config.ai.model,
                    "error": err.to_string(),
                    "elapsed_seconds": elapsed,
                    "runtime": runtime_payload,
                }),
            );
            (uid, ViewerStage::Failed(cache_tally))
        }
    }
}
