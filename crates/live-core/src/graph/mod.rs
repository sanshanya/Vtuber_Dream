//! 时序图谱子系统：store（SQLite schema v6 + upsert 语义，按写入类别拆 5 文件）、
//! build（Episode/AI 提交的受控写入）、query（search/references/episodes 读面基元）。

pub mod build;
pub mod query;
pub mod store;

pub use store::{GRAPH_QUERY_LIMIT, GRAPH_SCHEMA_VERSION, Store, StoreError};
