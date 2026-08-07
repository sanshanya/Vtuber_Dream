//! live-server：axum 服务 + run 注册表（装配层）。
//!
//! 模块边界（AGENTS.md 目标模块边界）：cli 只做解析分发 → lib 提供 app 装配；
//! 处理器只读/受控写 live-core 公开 API，不经手 figment。

pub mod app;
pub mod cytoscape;
pub mod graph_artifact;
pub mod registry;
pub mod ws_record;
