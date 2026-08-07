//! run 注册表 + spawn 通道（D3：内存 run registry + events 流）。
//!
//! 状态机照 design §10：
//! `queued → collecting → episodes → per_viewer_ai → audience → done | failed(partial)`
//! —— collecting/episodes 是由 spawn 线程主动焊上；per_viewer_ai/audience 由 live-core
//! pipeline 的 stage hook 上报（与 progress 文案解耦，AGENTS 不破坏既有测试签名）。
//!
//! 事件环形缓冲（live_core::events::RunEvents, cap 50）。

use std::sync::{Arc, Mutex};

use live_core::agent::budget::SpendMode;
use live_core::agent::pipeline::PipelineError;
use live_core::config::Config;
use live_core::episodes::{now_iso, now_unix_secs};
use live_core::events::RunEvents;
use live_core::live_ws::episodes::{WsRecorder, ingest_ws_window};
use live_core::live_ws::session::{WsSessionConfig, run_session};
use serde_json::{Value, json};

/// 状态序（design §10 枚举字面）。
/// 轮2-R2-2B（D1 WS 挂接）：`recording` 插在 collecting/episodes 之间——收集尾段
/// 探测在播时开 WS 场次窗，关窗后进既有 episodes 相（原状态机不破）。
pub const RUN_STATES: [&str; 8] = [
    "queued",
    "collecting",
    "recording",
    "episodes",
    "per_viewer_ai",
    "audience",
    "done",
    "failed",
];

/// POST 可提的 kind 值（D3）。
pub const RUN_KINDS: [&str; 2] = ["full", "viewer"];

/// Z4 动作平面：采集/AI 分层的四个新 kind（全名面，冻结给 api.ts 的 RunKind）。
/// collect_* 是事实层（不进 baseline/pipeline）；ai_* 是认知层（不进 collector——
/// AI 幂等靠 pipeline 既有 complete_cache(input_hash) 短路，不碰采集面）。
pub const RUN_KINDS_STAGED: [&str; 4] = [
    "collect_streamer",
    "collect_guards",
    "ai_viewers",
    "ai_audience",
];

fn utc_now() -> String {
    // 与 Python 时间戳同形（秒+微秒+ +00:00）；不新拉 chrono 进 live-server。
    live_core::episodes::now_iso()
}

pub struct RunRecord {
    pub run_id: String,
    /// full | viewer | demo。
    pub kind: String,
    pub viewer_uid: Option<String>,
    pub force: bool,
    /// RUN_STATES 中的当前状态字面。
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// 终态：complete 的纲领 JSON 或失败原因展示体。
    pub outcome: Option<Value>,
    pub partial: bool,
    pub events: Arc<RunEvents>,
}

#[derive(Default)]
struct RegistryInner {
    records: std::collections::HashMap<String, Arc<Mutex<RunRecord>>>,
    /// demo 静态快照：重复 POST /api/runs 返回同一记录（合成、无网络→幂等）。
    demo_snapshot_id: Option<String>,
}

#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
}

/// W1/r3-F2：登记簿保留上限——run 记录是进程内短时热数据，常驻只增不删会缓慢膨胀；
/// 终态记录按 started_at 从旧到新剔除至不越顶（在飞 run 永远保留，demo 快照被剔时也
/// 不影响幂等——demo_snapshot_id 兜底重栽，见 demo_snapshot）。
pub const RUN_RECORDS_CAP: usize = 64;

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, record: RunRecord) -> Arc<Mutex<RunRecord>> {
        // r3-F2：与 set_status/finalize 同一家纪律——状态字面只许出自 RUN_STATES。
        debug_assert!(
            RUN_STATES.contains(&record.status.as_str()),
            "未登记的 run 状态字面：{}（请补入 RUN_STATES）",
            record.status
        );
        let mut inner = self.inner.lock().expect("registry poisoned");
        Self::insert_locked(&mut inner, record)
    }

    /// 已持 inner 锁的登记通道：demo_snapshot / spawn_run 的复合判定+登记共享这一根。
    fn insert_locked(inner: &mut RegistryInner, record: RunRecord) -> Arc<Mutex<RunRecord>> {
        let shared = Arc::new(Mutex::new(record));
        let key = shared.lock().expect("record poisoned").run_id.clone();
        inner.records.insert(key, shared.clone());
        Self::gc_locked(inner);
        shared
    }

    /// 终态记录从旧到新剔除至不越 RUN_RECORDS_CAP；在飞 run 不参与剔除。
    fn gc_locked(inner: &mut RegistryInner) {
        if inner.records.len() <= RUN_RECORDS_CAP {
            return;
        }
        let mut terminal: Vec<(String, String)> = inner
            .records
            .values()
            .filter_map(|record| {
                let record = record.lock().expect("record poisoned");
                matches!(record.status.as_str(), "done" | "failed")
                    .then(|| (record.started_at.clone(), record.run_id.clone()))
            })
            .collect();
        // ISO 时间戳同形：字符串序 == 时间序。
        terminal.sort();
        let overflow = inner.records.len() - RUN_RECORDS_CAP;
        for (_, run_id) in terminal.into_iter().take(overflow) {
            inner.records.remove(&run_id);
        }
    }

    pub fn get(&self, run_id: &str) -> Option<Arc<Mutex<RunRecord>>> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .records
            .get(run_id)
            .cloned()
    }

    /// 记录总数（钉面：POST 校验副作用对账、GC 兑现验证）。
    pub fn record_count(&self) -> usize {
        self.inner.lock().expect("registry poisoned").records.len()
    }

    pub fn set_status(&self, run_id: &str, status: &str) {
        // D3/ag3-F4：状态字面必须来自 RUN_STATES——register 之外的第一消费点，debug 期撕破错拼。
        debug_assert!(
            RUN_STATES.contains(&status),
            "未登记的 run 状态字面：{status}（请补入 RUN_STATES）"
        );
        if let Some(record) = self.get(run_id) {
            record.lock().expect("record poisoned").status = status.to_string();
        }
    }

    /// 终局写入：done / failed(partial) + outcome + 时间戳。
    pub fn finalize(&self, run_id: &str, status: &str, outcome: Value, partial: bool) {
        debug_assert!(
            RUN_STATES.contains(&status),
            "未登记的 run 状态字面：{status}（请补入 RUN_STATES）"
        );
        if let Some(record) = self.get(run_id) {
            let mut record = record.lock().expect("record poisoned");
            record.status = status.to_string();
            record.outcome = Some(outcome);
            record.partial = partial;
            record.finished_at = Some(utc_now());
            record.events.push(&format!("[runs] 状态 → {status}"));
        }
    }

    /// demo 静态快照（G3 裁决）：合成、无网络、幂等——重复 POST /api/runs 返回同一记录。
    ///
    /// W1/r2-F4/r3-F3：查/栽/回填必须同锁——旧实现先查后栽，两个并发 POST 能各自栽出
    /// 一份快照并把 demo_snapshot_id 互相踩掉。快照记录若被 GC 剔走，id 兜底重栽。
    pub fn demo_snapshot(&self, outcome: Value) -> Arc<Mutex<RunRecord>> {
        let mut inner = self.inner.lock().expect("registry poisoned");
        if let Some(record) = inner
            .demo_snapshot_id
            .as_ref()
            .and_then(|id| inner.records.get(id))
        {
            return record.clone();
        }
        let shared = Self::insert_locked(
            &mut inner,
            RunRecord {
                run_id: format!("demo-{}", uuid::Uuid::new_v4()),
                kind: "demo".to_string(),
                viewer_uid: None,
                force: false,
                status: "done".to_string(),
                started_at: utc_now(),
                finished_at: Some(utc_now()),
                outcome: Some(json!({
                    "synthetic_demo": true,
                    "state": outcome,
                })),
                partial: false,
                events: Arc::new(RunEvents::new()),
            },
        );
        inner.demo_snapshot_id = Some(shared.lock().expect("record poisoned").run_id.clone());
        shared
    }

    /// 触发真实运行：spawn 一个普通线程（collect 是同步、pipeline 内有自身 runtime）。
    ///
    /// seam：`bilibili_hosts` 在测试面注入（wiremock）；生产 None → BilibiliClient::new。
    ///
    /// D3/ag3-F3 单 run 互斥：判定与登记在同一把 inner 锁内完成；已有未终态 run →
    /// Err(在飞 run_id)，由调用方折 409。demo 快照恒 done 不挡路。
    pub fn spawn_run(
        registry: &Registry,
        config: Config,
        kind: &str,
        viewer_uid: Option<String>,
        force: bool,
        spend_mode: SpendMode,
        bilibili_hosts: Option<(String, String)>,
    ) -> Result<Arc<Mutex<RunRecord>>, String> {
        let events = Arc::new(RunEvents::new());
        // ag3-F3：互斥判定 + 登记同锁，杜绝「双 POST 同时通过检查」窗口。
        let shared = {
            let mut inner = registry.inner.lock().expect("registry poisoned");
            if let Some(active) = inner.records.values().find_map(|candidate| {
                let record = candidate.lock().expect("record poisoned");
                (!matches!(record.status.as_str(), "done" | "failed"))
                    .then(|| record.run_id.clone())
            }) {
                return Err(active);
            }
            Self::insert_locked(
                &mut inner,
                RunRecord {
                    run_id: uuid::Uuid::new_v4().to_string(),
                    kind: kind.to_string(),
                    viewer_uid: viewer_uid.clone(),
                    force,
                    status: "queued".to_string(),
                    started_at: utc_now(),
                    finished_at: None,
                    outcome: None,
                    partial: false,
                    events: events.clone(),
                },
            )
        };
        let (run_id, started_at) = {
            let record = shared.lock().expect("record poisoned");
            (record.run_id.clone(), record.started_at.clone())
        };
        events.push("[runs] 已经进入队列…");

        let registry = registry.clone();
        let kind = kind.to_string();
        // 走账面（件3）需要在 config 被内层闭包整体吞掉前留一份 output_dir 克隆。
        let output_dir = config.output_dir.clone();
        // ag3-F2：catch_unwind 收尾自持三件套——线程体整体 move 走原句柄。
        let panic_registry = registry.clone();
        let panic_run_id = run_id.clone();
        let panic_events = events.clone();
        // 件3：panic 收尾分支在 body 整体 move 之后还要走账——kind/output_dir 各留一份。
        let panic_kind = kind.clone();
        let panic_output_dir = output_dir.clone();
        std::thread::spawn(move || {
            let body = move || {
                // emit 自持一份 Arc<RunEvents>——后续内层闭包要整体 move `events`，
                // 不能留下对它的借用（E0505）。
                let emit = {
                    let events = events.clone();
                    move |message: &str| events.push(message)
                };
                // 件3：compute 闭包整体 move `kind`/`emit`/`events`，终局收账前重造。
                let kind_for_outcome = kind.clone();
                let outcome_events = events.clone();
                emit(&format!("[runs] 触发 kind={kind}"));
                // Z4：动作平面分层——ai_* 只跑认知层（baseline+pipeline），不进 collector
                // 的 reset_output 屠刀（采集 + reset 会灭 ai/ 缓存，动作语义必须干净）。
                let ai_only = matches!(kind.as_str(), "ai_viewers" | "ai_audience");
                if !ai_only {
                    registry.set_status(&run_id, "collecting");
                }
                let mode = match kind.as_str() {
                    "viewer" => live_core::collector::run::CollectMode::SingleViewer(
                        viewer_uid.clone().unwrap_or_default(),
                    ),
                    "collect_streamer" => live_core::collector::run::CollectMode::StreamerOnly,
                    _ => live_core::collector::run::CollectMode::Guards,
                };
                let collect_only = kind.starts_with("collect_");
                let client = match &bilibili_hosts {
                    Some((api, live)) => live_core::bilibili::BilibiliClient::with_origin(
                        api,
                        live,
                        &config.bilibili.cookie,
                        config.collection.request_delay_seconds,
                        config.collection.timeout_seconds,
                    ),
                    None => live_core::bilibili::BilibiliClient::new(
                        &config.bilibili.cookie,
                        config.collection.request_delay_seconds,
                        config.collection.timeout_seconds,
                    ),
                };
                let state_dir = config.output_dir.join("ai/state.json");
                // 内层 outcome 闭包整体 move 走这两个 handle——外层收尾（finalize /
                // partial 判定）仍要用原版（E0382）。
                let inner_registry = registry.clone();
                let inner_run_id = run_id.clone();
                // 件2：闭包错误改携 PipelineError（保持原文案）——BudgetBlocked 需在
                // stringify 之前摘出带字段的阻断体，其余错误继续 to_string 落 outcome。
                let outcome: Result<Value, PipelineError> = (move || {
                    // 件3：内层 emit 就地重建——body 的 emit 留给终局收账（E0382 不重演）。
                    let emit = move |message: &str| outcome_events.push(message);
                    if !ai_only {
                        // 阶段①：collection
                        let client =
                            client.map_err(|error| PipelineError::Message(error.to_string()))?;
                        let mut emit_fn = |message: &str| emit(message);
                        let mut summary = live_core::collector::run::collect_with_client(
                            client,
                            &config,
                            mode,
                            &mut emit_fn,
                        )
                        .map_err(|error| PipelineError::Message(error.to_string()))?;
                        if collect_only {
                            // Z4a：collect_* 是事实层终局——collect_with_client 的汇总
                            // 即 outcome（无 viewer_failures 键 → 默认 0 → 非 partial）。
                            // 轮2-R2-2B（D1 挂接）：collect 尾段在播采录——live_ws_record==1
                            // 且房间在播时开 WS 弹幕窗（recording 相），窗线入账 graph
                            // （kind=ws-record，对照窗排除见 query.rs 头注），摘要并进
                            // outcome 的 ws_window 键；采录失败响铃不绊 run（采集产物
                            // 已落盘，弹幕窗是语料补充面）。旧命名「status=complete 已
                            // 冻结」不破——summary 从 collect_with_client 来，此处只增键。
                            match record_ws_window(
                                &config,
                                &bilibili_hosts,
                                &inner_registry,
                                &inner_run_id,
                                &emit,
                            ) {
                                Ok(Some(ws_window)) => {
                                    inner_registry.set_status(&inner_run_id, "episodes");
                                    emit(&format!(
                                        "[WS] 弹幕窗采录完成（{} 线入账，end_reason={}）",
                                        ws_window["lines"].as_i64().unwrap_or(0),
                                        ws_window["end_reason"].as_str().unwrap_or("unknown")
                                    ));
                                    let mut outcome = summary.clone();
                                    outcome["ws_window"] = ws_window;
                                    summary = outcome;
                                }
                                Ok(None) => {
                                    emit("[WS] 本轮未开弹幕窗（配置关/房间未在播）");
                                }
                                Err(error) => {
                                    emit(&format!("[WS] 弹幕窗采录失败：{error}"));
                                }
                            }
                            // P0-4（复盘解耦）：事实层终局顺带 T0 出卡——下播复盘四个数
                            // 是 shared/ 语料的纯规则聚合（零 AI），不再等全量感知；
                            // AI 命名仍属认知层（缺位 = null）。出卡失败响铃不绊 run
                            // （采集产物已落盘，卡只是呈现层读物）。
                            if let Err(err) =
                                live_core::recap::refresh_recap_card(&config.output_dir, &emit)
                            {
                                emit(&format!(
                                    "[RECAP] 复盘卡刷新失败（采集产物不受影响）：{err}"
                                ));
                            }
                            emit("[runs] 采集完成（collect_* 终局，未涉 AI 层）");
                            return Ok(summary);
                        }
                    }
                    // 阶段②：基线（episodes 风向：baseline 构造是唯一的显式调用点）
                    inner_registry.set_status(&inner_run_id, "episodes");
                    let analysis = live_core::episodes::baseline::build_factual_baseline(
                        &config.output_dir,
                        config.perception.max_evidence_per_viewer as usize,
                    )
                    .map_err(|error| PipelineError::Message(error.to_string()))?;
                    // 阶段③+④：pipeline——stage hook 进 registry；progress 进 events
                    let sink_events = events.clone();
                    let sink = move |message: &str| sink_events.push(message);
                    let stage_registry = inner_registry.clone();
                    let stage_id = inner_run_id.clone();
                    let stage_listener =
                        move |stage: &'static str| stage_registry.set_status(&stage_id, stage);
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| {
                            PipelineError::Message(format!("tokio runtime: {error}"))
                        })?;
                    rt.block_on(async move {
                        let mut knobs = live_core::agent::pipeline::PipelineKnobs {
                            progress: Some(&sink),
                            checkpoint: None,
                            apply_viewer: None,
                            bilibili_origin: bilibili_hosts.clone(),
                            stage: Some(&stage_listener),
                            // Z4b：ai_viewers 在 viewer 阶段写盘后收——不跑 audience。
                            stop_after_viewer_stage: kind_for_outcome == "ai_viewers",
                            // R2 批4 D3：省钱模式由 POST 动作面解析落位（Normal=默认全量）。
                            spend_mode,
                        };
                        match kind_for_outcome.as_str() {
                            "viewer" => {
                                live_core::agent::pipeline::run_viewer_pipeline(
                                    config,
                                    &analysis,
                                    viewer_uid.as_deref().unwrap_or_default(),
                                    force,
                                    &mut knobs,
                                )
                                .await
                            }
                            _ => {
                                live_core::agent::pipeline::run_pipeline(
                                    config, &analysis, force, &mut knobs,
                                )
                                .await
                            }
                        }
                    })
                })();
                // 件2：BudgetBlocked 先于 stringify 摘出阻断体；其余错误保持原 failed
                // 体（partial 判定数据源仍是 pipeline 契约键 viewer_stage_status）。
                match outcome {
                    Ok(value) => {
                        let partial = value["viewer_failures"].as_i64().unwrap_or(0) > 0;
                        registry.finalize(&run_id, "done", value, partial);
                        append_history_line(&output_dir, &run_id, &kind, spend_mode, "done", &emit);
                    }
                    Err(error) => match &error {
                        PipelineError::BudgetBlocked {
                            estimated_cny,
                            budget_cny,
                            fresh_viewers,
                            total_viewers,
                            spend_mode: blocked_spend_mode,
                        } => {
                            let outcome = json!({
                                "error": error.to_string(),
                                "budget_block": {
                                    "spend_mode": blocked_spend_mode.as_str(),
                                    "estimated_cny": estimated_cny,
                                    "budget_cny": budget_cny,
                                    "fresh_viewers": fresh_viewers,
                                    "total_viewers": total_viewers,
                                    "hint": "两选重发：spend_mode=incremental 只更新变化者 / briefing_only 只推简报",
                                },
                            });
                            emit("[BUDGET] 预估超预算，run 阻断（详见 outcome.budget_block）");
                            registry.finalize(&run_id, "failed", outcome, false);
                            append_history_line(
                                &output_dir,
                                &run_id,
                                &kind,
                                spend_mode,
                                "failed",
                                &emit,
                            );
                        }
                        _ => {
                            // partial 的数据源是 pipeline 契约键 viewer_stage_status（ag3-F1）。
                            // 注意 collect 期失败时上一轮 state 已被 collector 归档进
                            // history/snapshots，此处读到缺文件 → false，是正确语义。
                            // W1/r2-F5 时间闸：updated_at 早于本轮 started_at 的 complete
                            // 是旧轮次底票，不算本轮数据面（baseline/pipeline 期失败时
                            // collect 的旧 state 不再回来，窗口真实存在）。
                            let stage_complete =
                                viewer_stage_complete_since(&state_dir, &started_at);
                            registry.finalize(
                                &run_id,
                                "failed",
                                json!({"error": error.to_string()}),
                                stage_complete,
                            );
                            append_history_line(
                                &output_dir,
                                &run_id,
                                &kind,
                                spend_mode,
                                "failed",
                                &emit,
                            );
                        }
                    },
                }
            };
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
                // ag3-F2：spawn 线程恐慌不得静默弃档——记录 failed + 留 events 足迹。
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "非字符串 panic 载荷".to_string());
                panic_events.push(&format!("[runs] 线程恐慌，run 标记失败：{message}"));
                panic_registry.finalize(
                    &panic_run_id,
                    "failed",
                    json!({"error": format!("内部恐慌：{message}")}),
                    false,
                );
                append_history_line(
                    &panic_output_dir,
                    &panic_run_id,
                    &panic_kind,
                    spend_mode,
                    "failed",
                    &|message| panic_events.push(message),
                );
            }
        });
        Ok(shared)
    }
}

/// 轮2-R2-2B（D1 挂接）：collect 尾段的 WS 弹幕窗采录。
///
/// 流程（run 状态机 `collecting → recording → episodes`）：
/// 1. `live_ws_record==1` 且房间在播（`get_room_live_status==1`）才开窗；
/// 2. `get_danmu_info` 拿网关 host/port/token → `WsSessionConfig`（cookie 只进握手头）；
/// 3. current_thread runtime 跑 `run_session`，事件流直灌 `WsRecorder`；
/// 4. `finish` 收窗 → 窗线入账 graph（kind=ws-record；对照窗排除见 query.rs）→ 摘要。
///
/// 失败纪律：采录是语料补充面——任何一步失败响铃 `emit` 但不绊 run；返回
/// `Ok(None)` = 配置关/房间未在播（未开窗），`Ok(Some(Value))` = 窗摘要
/// （outcome 的 `ws_window` 键：lines / counts / end_reason / session / unknowns），
/// `Err` = 开窗后失败（摘要不产出，只有事件足迹）。
fn record_ws_window(
    config: &Config,
    bilibili_hosts: &Option<(String, String)>,
    registry: &Registry,
    run_id: &str,
    emit: &dyn Fn(&str),
) -> Result<Option<Value>, String> {
    if config.collection.live_ws_record != 1 {
        return Ok(None);
    }
    let Ok(room_id) = config.bilibili.room_id.parse::<i64>() else {
        emit(&format!(
            "[WS] 房间号非法（{}），跳过弹幕窗采录",
            config.bilibili.room_id
        ));
        return Ok(None);
    };
    emit(&format!(
        "[WS] live_ws_record 开启，探测房间 {room_id} 在播状态…"
    ));
    let client = match bilibili_hosts {
        Some((api, live)) => live_core::bilibili::BilibiliClient::with_origin(
            api,
            live,
            &config.bilibili.cookie,
            config.collection.request_delay_seconds,
            config.collection.timeout_seconds,
        ),
        None => live_core::bilibili::BilibiliClient::new(
            &config.bilibili.cookie,
            config.collection.request_delay_seconds,
            config.collection.timeout_seconds,
        ),
    }
    .map_err(|error| error.to_string())?;
    let mut client = client;

    let live_status = client
        .get_room_live_status(&config.bilibili.room_id)
        .map_err(|error| error.to_string())?;
    if live_status != 1 {
        emit(&format!(
            "[WS] 房间未在播（live_status={live_status}），跳过弹幕窗采录"
        ));
        return Ok(None);
    }

    emit("[WS] 房间在播，建立弹幕网关凭据…");
    let danmu = client
        .get_danmu_info(&config.bilibili.room_id)
        .map_err(|error| error.to_string())?;
    let mut session_cfg = WsSessionConfig::new(danmu.url(), room_id, danmu.token);
    // cookie 只进 WS 握手头（§11 红线：绝不进任何错误串/日志面）。
    session_cfg.cookie = config.bilibili.cookie.clone();

    let Some(mut recorder) = WsRecorder::attach(room_id, now_unix_secs(), 1) else {
        emit("[WS] 开窗失败（在播校验未过），跳过弹幕窗采录");
        return Ok(None);
    };
    registry.set_status(run_id, "recording");
    emit(&format!("[WS] 弹幕窗开启（rid={}，起点 attach）…", room_id));

    // 会话尽量跑：PREPARING 关窗 / 断连重连预算尽 / 12h 保险丝 / 认证失败。
    // 与 pipeline 同款 current_thread runtime（collect 是同步线程，不拉全局池）。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("tokio runtime: {error}"))?;
    let report = rt.block_on(run_session(
        &session_cfg,
        &mut |ev| {
            recorder.on_event(ev, now_unix_secs());
            Ok(())
        },
        &now_unix_secs,
    ))?;

    let capture = recorder.finish(&report.end, now_unix_secs());
    emit(&format!(
        "[WS] 弹幕窗收窗：end_reason={}，线 {} 条，未知 {} 行",
        capture.end_reason,
        capture.episodes.len(),
        capture.unknowns.len()
    ));

    // 窗线入账 graph（kind=ws-record，pipeline 数据面之外的账本容器——对照窗
    // 排除，语义注释见 graph/query.rs run_pair_delta）。
    match live_core::graph::Store::open(&config.output_dir.join("graph").join("perception.sqlite3"))
    {
        Ok(store) => {
            let graph_run = format!("run:{}", uuid::Uuid::new_v4().simple());
            if let Err(err) = store.begin_run_typed(
                &graph_run,
                &now_iso(),
                live_core::graph::Store::RUN_KIND_WS_RECORD,
                live_core::graph::Store::RUN_KIND_WS_RECORD,
                None,
            ) {
                emit(&format!("[WS] 弹幕窗 run 开账失败：{err}"));
            } else {
                match ingest_ws_window(&store, &graph_run, &capture) {
                    Ok(()) => {
                        if let Err(err) = store.complete_run(&graph_run) {
                            emit(&format!("[WS] 弹幕窗 run 结清失败：{err}"));
                        }
                    }
                    Err(err) => {
                        emit(&format!("[WS] 弹幕窗线入账失败：{err}"));
                        if let Err(close_err) = store.fail_run(&graph_run, &err.to_string(), false)
                        {
                            emit(&format!("[WS] 弹幕窗 run 记败失败：{close_err}"));
                        }
                    }
                }
            }
        }
        Err(err) => emit(&format!(
            "[WS] graph store 打开失败（弹幕窗线未入账）：{err}"
        )),
    }

    let ws_window = json!({
        "lines": capture.episodes.len(),
        "session": capture.session,
        "unknowns": capture.unknowns,
        "counts": capture.counts,
        "end_reason": capture.end_reason,
    });
    Ok(Some(ws_window))
}

/// ag3-F1：partial = viewer 阶段已完整走得一遍。数据源是 pipeline 契约键
/// `viewer_stage_status`（pipeline.rs fail_run_and_state 写 "complete"/"incomplete"）；
/// 宽松的 `status` 键恒为 "failed"/"interrupted"，任何 `status==viewer_complete`
/// 式读法都会假阴性（且若真有 status=="viewer_complete" 则假阳性）。
pub fn viewer_stage_complete(state_path: &std::path::Path) -> bool {
    std::fs::read_to_string(state_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|state| state["viewer_stage_status"].as_str().map(str::to_string))
        .as_deref()
        == Some("complete")
}

/// W1/r2-F5：partial 判定的时间闸——state.json 的 complete 必须是「本轮」的。
/// baseline/pipeline 期失败时 collect 前留下的旧 state.json 可能在原地侥幸带
/// viewer_stage_status=complete；`updated_at < started_at` 即旧票，不算本轮数据面。
/// ISO 时间戳同形：字符串序 == 时间序；缺 updated_at 按旧票处理（保守 false）。
pub fn viewer_stage_complete_since(state_path: &std::path::Path, started_at: &str) -> bool {
    let Some(state) = std::fs::read_to_string(state_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return false;
    };
    if state["viewer_stage_status"].as_str() != Some("complete") {
        return false;
    }
    state["updated_at"]
        .as_str()
        .is_some_and(|updated_at| updated_at >= started_at)
}

/// GET /api/runs/{id} 的回返形状（D3：轮询载体）。
pub fn run_to_json(record: &RunRecord) -> Value {
    json!({
        "run_id": record.run_id,
        "kind": record.kind,
        "viewer_uid": record.viewer_uid,
        "force": record.force,
        "status": record.status,
        "started_at": record.started_at,
        "finished_at": record.finished_at,
        "partial": record.partial,
        "outcome": record.outcome,
        "events": record.events.snapshot(),
    })
}

/// Z6 件3：run 到终态（done|failed，含恐慌收尾与预算阻断）即追加一行
/// `{output_dir}/history.jsonl`（append-only，一行一 JSON）。
///
/// 实耗语料 = `{output_dir}/ai/state.json` 的 usage 键（collect_* 无 AI → 全零）；
/// `cost_cny` 用 live-core 费率公式 `(input×2+output×8)/1_000_000`。state.json 缺失/
/// 坏 JSON → 实耗照记全零（诚实账本，不冒充成功）。写入失败只响铃 events，
/// 绝不改 run 终态——账本面永远让位给 run 状态机。
pub fn append_history_line(
    output_dir: &std::path::Path,
    run_id: &str,
    kind: &str,
    spend_mode: SpendMode,
    status: &str,
    emit: &dyn Fn(&str),
) {
    let usage = std::fs::read_to_string(output_dir.join("ai/state.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|state| state.get("usage").cloned())
        .unwrap_or_else(|| json!({}));
    let key = |name: &str| usage.get(name).and_then(Value::as_i64).unwrap_or(0);
    let input_tokens = key("input_tokens");
    let output_tokens = key("output_tokens");
    let line = json!({
        "ts": utc_now(),
        "run_id": run_id,
        "kind": kind,
        "spend_mode": spend_mode.as_str(),
        "status": status,
        "usage": {
            "llm_requests": key("llm_requests"),
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": key("total_tokens"),
        },
        "cost_cny": live_core::agent::budget::cost_cny(input_tokens, output_tokens),
    });
    let append = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output_dir.join("history.jsonl"))?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(&line).expect("history 行恒可 JSON")
        )?;
        Ok(())
    })();
    if let Err(error) = append {
        emit(&format!(
            "[LEDGER] history.jsonl 追加失败（run 终态不受影响）：{error}"
        ));
    }
}
