//! AudienceAnalysisAgent 编排（移植 `agent/pipeline.py`；Peer 链挂 G2）。
//!
//! 拆卷记（2026-08-07，二轮精修 C1）：原 1937 行单卷按职责收成五卷——
//! 旧「不拆」理由（Python parity golden 对账坐标系怕被拆散）随刀 8 判词失效；
//! 坐标系迁移方式 = 段标题与 parity 注记随代码各归其卷，golden 对账面
//! （tests/pipeline_inputs.rs × tests-fixtures/m4a/）经本壳全量 `pub use`
//! 再导出——外部 `use live_core::agent::pipeline::*` 坐标零改动。
//!
//! 职责地图：
//! - `cache` 认知缓存键族（M4-A 输入小件：stable_hash / viewer_input_bundle /
//!   episode_set_hash / fresh 判定 / roster 预估 / 时效位探针）
//! - `state` run 状态落盘（state.json 形状、usage 五键、Python except 兜底伞）
//! - `viewer` 单观众 agent 任务（缓存恢复 → 重校验 → run → 落盘）
//! - `audience` 整体态势（M4-A build_audience_input 两级封顶 + _run_audience 阶段体）
//! - `run` 编排主流程（M4-C 状态机字面名、入口二连、Knobs 接缝、
//!   扇出/栅栏/budget 闸/recap/reconcile 尾段）

mod audience;
mod cache;
mod run;
mod state;
mod viewer;

pub use audience::*;
pub use cache::*;
pub use run::*;
pub use state::*;
