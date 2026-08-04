//! run 注册表 + spawn 通道（D3：内存 run registry + events 流）。
//!
//! 状态机照 design §10：
//! `queued → collecting → episodes → per_viewer_ai → audience → done | failed(partial)`
//! —— collecting/episodes 是由 spawn 线程主动焊上；per_viewer_ai/audience 由 live-core
//! pipeline 的 stage hook 上报（与 progress 文案解耦，AGENTS 不破坏既有测试签名）。
//!
//! 事件环形缓冲（live_core::events::RunEvents, cap 50）。

use std::sync::{Arc, Mutex};

use live_core::config::Config;
use live_core::events::RunEvents;
use serde_json::{Value, json};

/// 状态序（design §10 枚举字面）。
pub const RUN_STATES: [&str; 7] = [
    "queued",
    "collecting",
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
        // ag3-F2：catch_unwind 收尾自持三件套——线程体整体 move 走原句柄。
        let panic_registry = registry.clone();
        let panic_run_id = run_id.clone();
        let panic_events = events.clone();
        std::thread::spawn(move || {
            let body = move || {
                // emit 自持一份 Arc<RunEvents>——后续内层闭包要整体 move `events`，
                // 不能留下对它的借用（E0505）。
                let emit = {
                    let events = events.clone();
                    move |message: &str| events.push(message)
                };
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
                let outcome: Result<Value, String> = (move || {
                    if !ai_only {
                        // 阶段①：collection
                        let client = client.map_err(|error| error.to_string())?;
                        let mut emit_fn = |message: &str| emit(message);
                        let summary = live_core::collector::run::collect_with_client(
                            client,
                            &config,
                            mode,
                            &mut emit_fn,
                        )
                        .map_err(|error| error.to_string())?;
                        if collect_only {
                            // Z4a：collect_* 是事实层终局——collect_with_client 的汇总
                            // 即 outcome（无 viewer_failures 键 → 默认 0 → 非 partial）。
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
                    .map_err(|error| error.to_string())?;
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
                        .map_err(|error| format!("tokio runtime: {error}"))?;
                    rt.block_on(async move {
                        let mut knobs = live_core::agent::pipeline::PipelineKnobs {
                            progress: Some(&sink),
                            checkpoint: None,
                            apply_viewer: None,
                            bilibili_origin: bilibili_hosts.clone(),
                            stage: Some(&stage_listener),
                            // Z4b：ai_viewers 在 viewer 阶段写盘后收——不跑 audience。
                            stop_after_viewer_stage: kind == "ai_viewers",
                        };
                        let result = match kind.as_str() {
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
                        };
                        result.map_err(|error| error.to_string())
                    })
                })();
                match outcome {
                    Ok(value) => {
                        let partial = value["viewer_failures"].as_i64().unwrap_or(0) > 0;
                        registry.finalize(&run_id, "done", value, partial);
                    }
                    Err(error) => {
                        // partial 的数据源是 pipeline 契约键 viewer_stage_status（ag3-F1）。
                        // 注意 collect 期失败时上一轮 state 已被 collector 归档进
                        // history/snapshots，此处读到缺文件 → false，是正确语义。
                        // W1/r2-F5 时间闸：updated_at 早于本轮 started_at 的 complete
                        // 是旧轮次底票，不算本轮数据面（baseline/pipeline 期失败时
                        // collect 的旧 state 不再回来，窗口真实存在）。
                        let stage_complete = viewer_stage_complete_since(&state_dir, &started_at);
                        registry.finalize(
                            &run_id,
                            "failed",
                            json!({"error": error}),
                            stage_complete,
                        );
                    }
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
            }
        });
        Ok(shared)
    }
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
