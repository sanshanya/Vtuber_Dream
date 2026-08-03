//! run 注册表（D3：内存 run registry + events 通道）。
//!
//! 状态机照 design §10：queued → collecting → episodes → per_viewer_ai → audience
//! → done | failed(partial)。spawn 执行本体在 B3；本文件先钉状态形状与闸
//! （events 环缓冲来源: live_core::events::RunEvents）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use live_core::events::RunEvents;
use serde_json::{Value, json};

/// 状态序（design §10 枚举字面；序列化钉在 app 层 oneshot 用例）。
pub const RUN_STATES: [&str; 7] = [
    "queued",
    "collecting",
    "episodes",
    "per_viewer_ai",
    "audience",
    "done",
    "failed",
];

pub struct RunRecord {
    pub run_id: String,
    /// full | viewer（kind=viewer 需带 viewer_uid，D7 单查通道）。
    pub kind: String,
    pub viewer_uid: Option<String>,
    pub force: bool,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// 终态挂 outcome（complete 的纲领 JSON 或失败原因展示体）。
    pub outcome: Option<Value>,
    pub events: Arc<RunEvents>,
}

#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<HashMap<String, std::sync::Arc<Mutex<RunRecord>>>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, record: RunRecord) -> std::sync::Arc<Mutex<RunRecord>> {
        let shared = std::sync::Arc::new(Mutex::new(record));
        self.inner.lock().expect("registry poisoned").insert(
            shared.lock().expect("record poisoned").run_id.clone(),
            shared.clone(),
        );
        shared
    }

    pub fn get(&self, run_id: &str) -> Option<std::sync::Arc<Mutex<RunRecord>>> {
        self.inner
            .lock()
            .expect("registry poisoned")
            .get(run_id)
            .cloned()
    }

    pub fn set_status(&self, run_id: &str, status: &str) {
        if let Some(record) = self.get(run_id) {
            record.lock().expect("record poisoned").status = status.to_string();
        }
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
        "outcome": record.outcome,
        "events": record.events.snapshot(),
    })
}
