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

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, record: RunRecord) -> Arc<Mutex<RunRecord>> {
        let shared = Arc::new(Mutex::new(record));
        let key = shared.lock().expect("record poisoned").run_id.clone();
        self.inner
            .lock()
            .expect("registry poisoned")
            .records
            .insert(key, shared.clone());
        shared
    }

    pub fn get(&self, run_id: &str) -> Option<Arc<Mutex<RunRecord>>> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .records
            .get(run_id)
            .cloned()
    }

    pub fn set_status(&self, run_id: &str, status: &str) {
        if let Some(record) = self.get(run_id) {
            record.lock().expect("record poisoned").status = status.to_string();
        }
    }

    /// 终局写入：done / failed(partial) + outcome + 时间戳。
    pub fn finalize(&self, run_id: &str, status: &str, outcome: Value, partial: bool) {
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
    pub fn demo_snapshot(&self, outcome: Value) -> Arc<Mutex<RunRecord>> {
        let existing = {
            let inner = self.inner.lock().expect("registry poisoned");
            inner.demo_snapshot_id.clone()
        };
        if let Some(id) = existing {
            return self.get(&id).expect("demo snapshot registered");
        }
        let record = self.register(RunRecord {
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
        });
        let id = record.lock().expect("record poisoned").run_id.clone();
        self.inner
            .lock()
            .expect("registry poisoned")
            .demo_snapshot_id = Some(id);
        record
    }

    /// 触发真实运行：spawn 一个普通线程（collect 是同步、pipeline 内有自身 runtime）。
    ///
    /// seam：`bilibili_hosts` 在测试面注入（wiremock）；生产 None → BilibiliClient::new。
    pub fn spawn_run(
        registry: &Registry,
        config: Config,
        kind: &str,
        viewer_uid: Option<String>,
        force: bool,
        bilibili_hosts: Option<(String, String)>,
    ) -> Arc<Mutex<RunRecord>> {
        let events = Arc::new(RunEvents::new());
        let record = registry.register(RunRecord {
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
        });
        let run_id = record.lock().expect("record poisoned").run_id.clone();
        events.push("[runs] 已经进入队列…");

        let registry = registry.clone();
        let kind = kind.to_string();
        std::thread::spawn(move || {
            // emit 自持一份 Arc<RunEvents>——后续内层闭包要整体 move `events`，
            // 不能留下对它的借用（E0505）。
            let emit = {
                let events = events.clone();
                move |message: &str| events.push(message)
            };
            emit(&format!("[runs] 触发 kind={kind}"));
            registry.set_status(&run_id, "collecting");
            let mode = match kind.as_str() {
                "viewer" => live_core::collector::run::CollectMode::SingleViewer(
                    viewer_uid.clone().unwrap_or_default(),
                ),
                _ => live_core::collector::run::CollectMode::Guards,
            };
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
            // 内层 outcome 闭包整体 move 走这两个 handle——外层收尾（finalize / partial
            // 判定）仍要用原版（E0382）。
            let inner_registry = registry.clone();
            let inner_run_id = run_id.clone();
            let outcome: Result<Value, String> = (move || {
                // 阶段①：collection
                let client = client.map_err(|error| error.to_string())?;
                let mut emit_fn = |message: &str| emit(message);
                live_core::collector::run::collect_with_client(client, &config, mode, &mut emit_fn)
                    .map_err(|error| error.to_string())?;
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
                    // partial = viewer 阶段已完整走得一遍（写 viewer_complete 后 audience 期失败）。
                    let stage_complete = std::fs::read_to_string(&state_dir)
                        .ok()
                        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                        .and_then(|state| state["status"].as_str().map(str::to_string))
                        .as_deref()
                        == Some("viewer_complete");
                    registry.finalize(&run_id, "failed", json!({"error": error}), stage_complete);
                }
            }
        });
        record
    }
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
