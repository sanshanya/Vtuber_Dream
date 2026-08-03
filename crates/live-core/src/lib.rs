//! live-core: 引擎纯库（不依赖 axum/tauri）。
//!
//! 模块随里程碑挂入（对照 docs/2026-08-03-rust-rewrite-design.md §4）：
//! - M1：config / episodes / models / graph（schema v6 + INTERESTED_IN 幂等）。
//! - M2：bilibili（HTTP 适配 + WBI）/ collector（normalize + 编排）/ storage（文件面）。

pub const PROTOCOL_NOTE: &str =
    "事实、推断、状态、行动必须分层；普通 assistant 文本不是程序输出（AGENTS.md §2）";

pub mod bilibili;
pub mod collector;
pub mod config;
pub mod episodes;
pub mod graph;
pub mod models;
pub mod storage;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_note_is_pinned() {
        assert!(PROTOCOL_NOTE.contains("分层"));
    }
}
