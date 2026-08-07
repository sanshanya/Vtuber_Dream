//! 编排主流程（M4-C 阶段状态机 `AudienceAnalysisAgent.run_async` 平移）。
//!
//! 入口二连：`run_pipeline`（全量：per_viewer_ai → audience → recap/reconcile 尾段）
//! 与 `run_viewer_pipeline`（单观众薄封装）。`PipelineKnobs` 是可插接缝
//! （不抽象化：各一个真实默认 + 测试覆盖点）；阶段字面名只由本卷常量公布——
//! queued/collecting/episodes/done/failed 由调用方（live-server registry）控制。
//! `run_pipeline_inner` 只做相位排序与兜底伞；名册捆/扇出栅栏/尾段各段已下沉为
//! 具名单元（prep_roster / stage_per_viewer_ai / recap_finale / reconcile_finale）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;

use super::super::budget;
use super::super::naming;
use super::super::reconcile;
use super::super::runtime::AgentRuntime;
use super::super::throttle::Throttle;
use super::super::tools::ResearchService;
use super::audience::run_audience_stage;
use super::cache::{ViewerInputBundle, fresh_viewer_ids, reasoning_json, viewer_input_bundle};
use super::state::{
    UmbrellaNote, aggregate_runtime_usage, cache_usage_json, fail_run_and_state, utc_now,
    write_state,
};
use super::viewer::{ViewerStage, run_one_viewer};
use crate::bilibili::BilibiliClient;
use crate::config::Config;
use crate::episodes::{self, Episode};
use crate::graph::build;
use crate::graph::project::{ProjectOptions, project};
use crate::graph::store::{Store, StoreError};
use crate::leads;
use crate::models::{AudienceSituationSubmission, ViewerPerceptionSubmission};
use crate::recap;
use crate::storage::{self, load_viewers};

/// Python asyncio.Semaphore(4)（ADR-0004 同构）。
/// 作为 config `ai.agent.max_parallel_viewers` 的默认锚保留；
/// 实际并发上限由 config 决定（pipeline 扇出处消费），本常量不再直接驱动 Semaphore。
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
    /// 花费预算闸阻断。块内附预估/预算/名册数字（wire 文案首字「budget_blocked」
    /// 供运行概览归类）。出路只有两条：调 `ai.run_budget_cny`，或先跑舰长采集让
    /// 缓存变新（fresh 随 input_hash 失效面缩小）——不再提供模式侧门。
    #[error(
        "budget_blocked：预估 ¥{estimated_cny:.2} > 预算 ¥{budget_cny:.2}（新鲜 {fresh_viewers}/{total_viewers} 人）"
    )]
    BudgetBlocked {
        estimated_cny: f64,
        budget_cny: f64,
        fresh_viewers: usize,
        total_viewers: usize,
    },
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
    /// 通道可失败——Err 不穿透（Python 吞异常记 progress 同型）。
    pub checkpoint: Option<&'a mut dyn FnMut() -> Result<(), String>>,
    /// 测试接缝：graph_failed 分支的确定性复现（默认 build::apply_viewer_submission）。
    pub apply_viewer: Option<&'a mut ApplyViewerFn<'a>>,
    /// 测试接缝：Bilibili 回放根地址（默认官方端点；调查工具发起网络时才消费）。
    pub bilibili_origin: Option<(String, String)>,
    /// M5-B3：run 状态机 stage hook。已公布的字面名只来自本文件两个常量；
    /// queued/collecting/episodes/done/failed 由调用方（live-server registry）控制——
    /// 显式 seam 使 progress 文案与状态机解耦（不改既有测试签名：Option<'_'> 字段）。
    pub stage: Option<&'a dyn Fn(&'static str)>,
    /// 动作平面 kind=ai_viewers：viewer 阶段写盘完成即收——不跑 audience。
    /// 语义边界：situation.json 保持上一轮；终盘 state.json 落 `stage_terminal=per_viewer_ai`。
    pub stop_after_viewer_stage: bool,
}

pub(crate) fn progress_say(knobs: &PipelineKnobs<'_>, message: &str) {
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

/// M4.x → G2 表形态（design §9.2 行 254）：leads 账本摘要注入用户消息
///（hash 之外——账本漂移不是 Python parity 的输入身份，缓存有效性不因此失效；
/// 登记 design-Δ）。数据源 = discovery_leads 表；账本为空则零面世（首跑提示面
/// 与 M4 逐字节一致）。读库失败 → 吞纳零面世（annex 是上下文增益面，不绊管线）。
pub(crate) fn ledger_annex(store: &Store, viewer: Option<&str>, prompt: String) -> String {
    let rows = leads::read_rows(store).unwrap_or_default();
    if rows.is_empty() {
        return prompt;
    }
    let mut prompt = format!("{prompt}\n\n{}", leads::summary_line(&rows, viewer));
    // 事实密度 annex（迭代细则 v1 §1）：每 consumed lead 的观众画像数
    // + 证据摘要（行尾 episode 回链）。增益面纪律同人 comment：读错吞纳不绊管线。
    if let Ok(lines) = leads::consumed_annex_lines(store, &rows)
        && !lines.is_empty()
    {
        prompt.push('\n');
        prompt.push_str(&lines.join("\n"));
    }
    // 拒绝回喂聚合线（零被拒 → None：旧版逐字节不变；rejected 只
    // 折叠白名单 chip 计数 + 最近注记截字——平台事实不可被 AI 改写，仅携带）。
    if let Ok(Some(line)) = leads::reject_annex_line(store) {
        prompt.push('\n');
        prompt.push_str(&line);
    }
    prompt
}

pub(crate) fn graph_file(root: &Path) -> PathBuf {
    root.join("graph").join("perception.sqlite3")
}

pub(crate) fn new_client(
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

// ---------------------------------------------------------------------------
// 名册捆预备（inner 的 try-前面——本单元 Err 由 inner 折回 (Err, master) 裸传播）
// ---------------------------------------------------------------------------

/// prep 段产物：名册三件事 + 每观众输入捆 + 哈希账面（兜底伞/预算闸/state 共用）。
struct RosterPrep {
    raw_viewers: Map<String, Value>,
    baseline_profiles: Vec<Value>,
    viewer_ids: Vec<String>,
    bundles: HashMap<String, ViewerInputBundle>,
    viewer_input_hashes: Map<String, Value>,
}

fn prep_roster(config: &Config, analysis: &Value) -> Result<RosterPrep, PipelineError> {
    let raw_viewers: Map<String, Value> = load_viewers(&config.output_dir)
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
    Ok(RosterPrep {
        raw_viewers,
        baseline_profiles,
        viewer_ids,
        bundles,
        viewer_input_hashes,
    })
}

// ---------------------------------------------------------------------------
// per_viewer_ai 相位体（并发扇出 + 有序应用栅栏）
// ---------------------------------------------------------------------------

/// M4-C per_viewer_ai 相位体：并发扇出（Semaphore 闸门一）+ viewer_ids 序应用栅栏。
/// Queued/collecting/episodes 由 registry 自己直接进 per_viewer_ai；本卷只经
/// knobs.stage 上报 AI 两相字面名。
/// 返回（按名册序合并的提交面, graph 失败明细, 本轮 prompt-cache 计量）。
/// 「空提交 = 全员失败」的兜底伞裁决留在 inner（bail! 的承保域在编排层）。
#[allow(clippy::too_many_arguments)]
async fn stage_per_viewer_ai(
    knobs: &mut PipelineKnobs<'_>,
    config: &Config,
    runtime: &Arc<AgentRuntime>,
    store: &Store,
    run_id: &str,
    force: bool,
    ai_root: &Path,
    viewer_cache_dir: &Path,
    graph_file: &Path,
    reasoning: &Value,
    raw_viewers: &Map<String, Value>,
    baseline_profiles: &[Value],
    viewer_ids: &[String],
    bundles: &mut HashMap<String, ViewerInputBundle>,
    master: &mut ResearchService,
) -> (Map<String, Value>, Vec<Value>, (i64, i64)) {
    // 闸门一：并行 viewer 数由 config 驱动（默认锚 = INVESTIGATE_CONCURRENCY）。
    let semaphore = Arc::new(Semaphore::new(
        config.ai.agent.max_parallel_viewers.max(1) as usize
    ));
    let mut set: tokio::task::JoinSet<(String, ViewerStage)> = tokio::task::JoinSet::new();
    // 扇出 = 名册全员；变化判断由缓存短路（complete_cache）在行内完成——
    // 未变者零 LLM 复用旧结论，与预算闸 fresh 口径互补成账。
    for uid in viewer_ids {
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
            graph_file.to_path_buf(),
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
    // viewer 阶段本轮 LLM 的 prompt-cache 入账（复用臂不贡献——诚实零）。
    let mut viewer_cache_tally: (i64, i64) = (0, 0);
    for uid in viewer_ids {
        let Some(stage) = outputs.remove(uid) else {
            continue;
        };
        let analysis = match stage {
            ViewerStage::Ok(out) => {
                master.absorb_from(&out.child);
                viewer_cache_tally.0 += out.cache_tally.0;
                viewer_cache_tally.1 += out.cache_tally.1;
                let child = out.child;
                tokio::task::spawn_blocking(move || drop(child))
                    .await
                    .expect("spawn join");
                out.analysis
            }
            ViewerStage::Reused(analysis) => analysis,
            ViewerStage::Failed(tally) => {
                viewer_cache_tally.0 += tally.0;
                viewer_cache_tally.1 += tally.1;
                continue;
            }
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
            reasoning,
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
            Some(hook) => hook(store, run_id, &name, &rebuilt.episodes, &submission),
            None => {
                build::apply_viewer_submission(store, run_id, &name, &rebuilt.episodes, &submission)
            }
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
        // M4.x：Ok/Reused 双臂在 apply 成功点汇流——账本补写幂等，
        // 缓存命中路径同样补账（首跑中断后恢复不丢账）。
        // G2 表形态（design §9.2）：dedication 表 OR IGNORE 幂等，唯一键即 dedupe_key。
        // fail-open 保持但响铃——线索无痕蒸发 ≠ 豁免项。
        if let Err(err) = leads::record_leads(store, uid, run_id, &utc_now(), &submission.leads) {
            progress_say(knobs, &format!("[LEADS] 观众 {uid} 账本写入失败：{err}"));
        }
        if let Some(checkpoint) = knobs.checkpoint.as_deref_mut() {
            // 报告刷新失败必须与 Python 同型——吞掉记 progress，不打断管线。
            if let Err(err) = checkpoint() {
                progress_say(knobs, &format!("[AI] 观众 {uid} checkpoint 失败：{err}"));
            }
        }
    }
    (viewer_submissions, graph_failures, viewer_cache_tally)
}

// ---------------------------------------------------------------------------
// 入口二连
// ---------------------------------------------------------------------------

/// 主体（Python `run_async`）。ctrl-c → aborted=true 收尾。
pub async fn run_pipeline(
    config: Config,
    analysis: &Value,
    force: bool,
    knobs: &mut PipelineKnobs<'_>,
) -> Result<Value, PipelineError> {
    let runtime = Arc::new(
        AgentRuntime::from_ai_config(&config.ai)
            .map_err(|err| PipelineError::Message(err.to_string()))?
            // 闸门二：run 级限速漏桶（max_llm_rpm=0 → Throttle::disabled，空操作）。
            // viewer 任务克隆 Arc<AgentRuntime> 共享同一桶；audience 臂同一 runtime 引用。
            .with_throttle(Arc::new(Throttle::build(config.ai.agent.max_llm_rpm))),
    );
    // Python 次序：research(master) 在 begin_graph_run 之前构造；其失败走裸传播（不 umbrella）。
    // 含 blocking client → 构造与处置都走 spawn_blocking。
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

/// M5-B3：单观众薄封装——analysis 的 viewer_profiles 过滤后走同一 run_pipeline
/// 主体；viewer 未落入 baseline → 明确错误，不新写编排。
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

// ---------------------------------------------------------------------------
// 相位排序器（兜底伞承保域）
// ---------------------------------------------------------------------------

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
    let prep = match prep_roster(config, analysis) {
        Ok(prep) => prep,
        Err(err) => return (Err(err), master),
    };
    let RosterPrep {
        raw_viewers,
        baseline_profiles,
        viewer_ids,
        mut bundles,
        viewer_input_hashes,
    } = prep;
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
    let run_id = match store.begin_run(&config.ai.model) {
        Ok(run_id) => run_id,
        Err(err) => bail!(PipelineError::Store(err)),
    };
    note.run_id = Some(run_id.clone());
    progress_say(knobs, "[GRAPH] 写入Episode、Mention、Entity和兴趣状态");
    // 房间语料 Episode 化收账（迭代细则 v1 §1）：collect 只写 shared/*.json
    // （replay_danmaku / room_comments），这里在观众扇出前转 Episode 走既有 ingest
    // 通道落图（viewer 命名空间 = _room）。幂等语义继承 upsert_episode_inner；
    // 入账失败是独立工作单元：响铃不绊管线（已完成的观众结果不受影响）。
    {
        let (corpus, corpus_counts) =
            episodes::room_corpus::room_corpus_episodes(&config.output_dir.join("shared"));
        let danmaku_count = corpus_counts["live_danmaku"].as_i64().unwrap_or(0);
        let comment_count = corpus_counts["room_comment"].as_i64().unwrap_or(0);
        match episodes::room_corpus::ingest_room_corpus(&store, &run_id, &corpus) {
            Ok(()) => progress_say(
                knobs,
                &format!(
                    "[GRAPH] 房间语料入账：弹幕 {danmaku_count} 行、评论 {comment_count} 条（_room 命名空间）"
                ),
            ),
            Err(err) => progress_say(
                knobs,
                &format!(
                    "[GRAPH] 房间语料入账失败：{err}（弹幕 {danmaku_count}、评论 {comment_count} 待重跑恢复）"
                ),
            ),
        }
    }
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
    // 花费预算闸：先于扇出决定 fresh 集合与放行/阻断——预估严格大于预算即
    // 阻断，零 LLM 请求落地。口径与执行同源（complete_cache 短路的补集），
    // 不再存在「名册上限估计阻断实际零花费 run」的负循环，也无需模式侧门逃生。
    let fresh = fresh_viewer_ids(&viewer_ids, &bundles, &viewer_cache_dir);
    {
        let check = budget::decide_budget(fresh.len(), true, config.ai.run_budget_cny);
        let budget_text = match check.budget_cny {
            Some(budget) => format!("¥{budget:.2}"),
            None => "不设".to_string(),
        };
        progress_say(
            knobs,
            &format!(
                "[BUDGET] 名册={} 新鲜={} 预估≈¥{:.2} 预算={} → {}",
                viewer_ids.len(),
                fresh.len(),
                check.estimated_cny,
                budget_text,
                if check.blocked { "阻断" } else { "放行" },
            ),
        );
        if check.blocked {
            bail!(PipelineError::BudgetBlocked {
                estimated_cny: check.estimated_cny,
                budget_cny: check.budget_cny.expect("blocked 必携预算"),
                fresh_viewers: fresh.len(),
                total_viewers: viewer_ids.len(),
            });
        }
    }
    stage_say(knobs, STAGE_PER_VIEWER_AI);
    let (viewer_submissions, graph_failures, viewer_cache_tally) = stage_per_viewer_ai(
        knobs,
        config,
        runtime,
        &store,
        &run_id,
        force,
        &ai_root,
        &viewer_cache_dir,
        &graph_file,
        &reasoning_json(config),
        &raw_viewers,
        &baseline_profiles,
        &viewer_ids,
        &mut bundles,
        master.as_mut().expect("master 在场"),
    )
    .await;
    // 栅栏后校验：空提交 = 全员失败（真故障面——文案维持既有语义）。
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
            limit: Some(config.perception.graph_row_limit),
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
    // 动作平面「舰长 AI 分析」kind：viewer 阶段全部落盘即收——不进 audience；
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
        let cache_usage = cache_usage_json(viewer_cache_tally, (0, 0));
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
                "cache_usage": cache_usage,
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
            "cache_usage": cache_usage,
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
    let (overall, overall_runtime, audience_cache_tally) = match audience {
        Ok(triple) => triple,
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
        //（apply 成功后，同 viewer 纪律）。fail-open 保持但响铃。
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
    recap_finale(config, runtime, knobs).await;
    reconcile_finale(config, runtime, &store, &run_started_at, knobs).await;
    let usage = aggregate_runtime_usage(&viewer_runtime, &overall_runtime);
    let cache_usage = cache_usage_json(viewer_cache_tally, audience_cache_tally);
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
            // design-Δ：token 成本一等公民入 state.json。
            "usage": usage,
            // cache 观测盒姊妹键（usage 保 Python 五键 parity）。
            "cache_usage": cache_usage,
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
        "cache_usage": cache_usage,
    });
    (Ok(final_result), master.take().expect("master 归还"))
}

// ---------------------------------------------------------------------------
// 尾段（complete_run 之后的聚合层润色——子失败响铃不绊管线）
// ---------------------------------------------------------------------------

/// 复盘解耦（迭代细则 v1 §1）：四个数 + 旧命名留存/作废判定全沉
/// 进 recap::refresh_recap_card（collect 尾同门进出——出卡不再锁全量感知）。
/// 本处只叠 AI 命名窗：卡 ready 且命名缺位才跑 naming 一击终局——同场次同数面
/// 的旧命名已被 refresh 留存，不白跑 AI。子失败响铃不绊管线（图与 situation
/// 已落的事实不受影响；AI 命名缺位 = naming:null + 未知行）。
async fn recap_finale(config: &Config, runtime: &Arc<AgentRuntime>, knobs: &PipelineKnobs<'_>) {
    match recap::refresh_recap_card(&config.output_dir, &|msg| progress_say(knobs, msg)) {
        Ok(mut card) => {
            let need_naming = card.status == "ready"
                && card.naming.is_none()
                && (card.peak.is_some() || card.repeated.is_some());
            if need_naming {
                match naming::run_recap_naming(runtime, config, &card).await {
                    Ok(named) => {
                        progress_say(knobs, "[RECAP] AI 命名落卡");
                        card.naming = Some(named);
                    }
                    Err(err) => {
                        progress_say(knobs, &format!("[RECAP] AI 命名未达成：{err}"));
                        card.unknown.push(format!("AI 命名未达成：{err}"));
                    }
                }
                if let Err(err) = recap::write_recap_card(&config.output_dir, &card) {
                    progress_say(knobs, &format!("[RECAP] 复盘卡落盘失败：{err}"));
                }
            }
        }
        Err(err) => progress_say(knobs, &format!("[RECAP] 复盘卡计算失败：{err}")),
    }
}

/// 用户裁决（AI 看图裁决归并，程序出纳）：实体「AI 归并」管道尾门。
/// 本轮确实铸起了新实体（minted>0）才值得派归并 Agent 出场——minted=0 表示
/// 本轮对实体事实面零触碰，归并无对象，跳过（保持零模型调用静默性）。
/// 归并失败只响铃不绊管线：图事实、证据、账本均已落，归并属于聚合层润色。
async fn reconcile_finale(
    config: &Config,
    runtime: &Arc<AgentRuntime>,
    store: &Store,
    run_started_at: &str,
    knobs: &PipelineKnobs<'_>,
) {
    let minted = store
        .count_scalar(
            "SELECT COUNT(*) FROM entities WHERE first_seen_at >= ?1",
            &[rusqlite::types::Value::Text(run_started_at.to_string())],
        )
        .unwrap_or(0);
    if minted > 0 && !knobs.stop_after_viewer_stage {
        match reconcile::run_entity_reconcile(runtime, config, store).await {
            Ok(report) => progress_say(
                knobs,
                &format!(
                    "[RECONCILE] 实体归并完成：merge {} 组 / drop {} 个 / 失败 {} 项",
                    report.merged_ok,
                    report.dropped_ok,
                    report.failed.len()
                ),
            ),
            Err(err) => {
                progress_say(knobs, &format!("[RECONCILE] 实体归并未达成：{err}"));
            }
        }
    }
}
