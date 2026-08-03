//! M5 G1：run events 环形缓冲（design §10：`GET /api/runs/{id}` 的 events[]
//! 保最近条数，progress_say 回调接流）。
//!
//! 单 run 一份；serve 的内存 run registry 持有。线程安全（pipeline 观众级并发 +
//! registry 查询并存），钳制语义钉在模块内测试。此面只是缓冲 + sink 适配——
//! knobs.progress 的现有签名（`&dyn Fn(&str)`）不动（kickoff G1 约束：不破坏
//! M4.x 既有测试）。

use std::collections::VecDeque;
use std::sync::Mutex;

/// design §10：events[] 保留上限（钉：last_n_capped）。
pub const RUN_EVENTS_CAP: usize = 50;

#[derive(Default)]
pub struct RunEvents {
    inner: Mutex<VecDeque<String>>,
}

impl RunEvents {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, message: &str) {
        let mut buffer = self.inner.lock().expect("events lock not poisoned");
        buffer.push_back(message.to_string());
        while buffer.len() > RUN_EVENTS_CAP {
            buffer.pop_front();
        }
    }

    /// 插入序快照（API 直接回传：轮询面不做分页）。
    pub fn snapshot(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("events lock not poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("events lock not poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 适配 `PipelineKnobs.progress: Option<&dyn Fn(&str)>`：零签名改动接入。
    pub fn sink(&self) -> impl Fn(&str) + '_ {
        move |message: &str| self.push(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_n_capped_and_ordered() {
        let events = RunEvents::new();
        for i in 0..(RUN_EVENTS_CAP + 10) {
            events.push(&format!("msg-{i}"));
        }
        assert_eq!(events.len(), RUN_EVENTS_CAP);
        let snapshot = events.snapshot();
        // 最旧的 10 条被挤掉，保留的仍按插入序排列。
        assert_eq!(snapshot.first().map(String::as_str), Some("msg-10"));
        assert_eq!(
            snapshot.last().map(String::as_str),
            Some(format!("msg-{}", RUN_EVENTS_CAP + 9).as_str())
        );
    }

    #[test]
    fn sink_adapter_and_empty_contract() {
        let events = RunEvents::new();
        assert!(events.is_empty());
        let sink = events.sink();
        sink("audience 开始");
        assert_eq!(events.snapshot(), vec!["audience 开始".to_string()]);
    }

    #[test]
    fn concurrent_writers_no_loss_no_panic() {
        let events = std::sync::Arc::new(RunEvents::new());
        let mut handles = Vec::new();
        for t in 0..8 {
            let events = events.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    events.push(&format!("t{t}-{i}"));
                }
            }));
        }
        for handle in handles {
            handle.join().expect("writer thread");
        }
        assert_eq!(events.len(), RUN_EVENTS_CAP);
    }
}
