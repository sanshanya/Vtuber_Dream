//! 「WS 弹幕窗」。分两段：
//! - 第一段：协议 codec 层（字节/JSON，零网络）。
//! - 第二段 2A：`session`——WS 会话录制引擎（会话状态机 + 事件输出回调）。
//! - 第二段 2B：`episodes`——场次窗 Episode 化（WS 事件 → 弹幕/SC/进场 Episode，
//!   `finalize_episode` 同指纹撞库；事实键纪律：不产现货实体三键）。
//!
//! 协议真理源：bilibili-API-collect `docs/live/message_stream.md`（执行单
//! `docs/2026-08-06-r2-execution-spec.md` 已核对；本层只认这一份事实源）。
//! 合成字节流 fixture 即规格：codec/message 验收钉在 `crates/live-core/tests/live_ws_codec.rs`，
//! 会话引擎验收钉在 `crates/live-core/tests/live_ws_session.rs`。
//!
//! 时序规格来源=执行单（第二段 2A「WS 会话录制引擎」逐条工程化）。
//!
//! 四个文件的分工（体积纪律：单文件 >500 行必分）：
//! - `codec`：16 字节大端头切包/组装、版本 2/3 解压多包展开（字节层）。
//! - `message`：op=5 行下 JSON → 最小消息族事件、op=3/op=8 回复、认证/心跳构造器。
//! - `session`：认证 5s / 心跳 30s±60s 判死 / 断线指数退避重连 / 同窗续接 /
//!   PREPARING 关窗 / 12h 保险丝的会话状态机与事件回调。
//! - `episodes`：场次窗 + 线→Episode 投影 + `ingest_ws_window` 入账通道。

pub mod codec;
pub mod episodes;
pub mod message;
pub mod session;
