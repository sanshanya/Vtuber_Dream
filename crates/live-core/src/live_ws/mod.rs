//! R2 批 2 D1「WS 弹幕窗」—— 第一段：协议 codec 层（纯字节/JSON，零网络）。
//!
//! 协议真理源：bilibili-API-collect `docs/live/message_stream.md`（R2 执行单
//! `docs/2026-08-06-r2-execution-spec.md` 批 2 已核对；本层只认这一份事实源）。
//! 合成字节流 fixture 即规格：全部验收钉在 `crates/live-core/tests/live_ws_codec.rs`。
//!
//! 本段只产出**结构化事件**（`codec::RawPacket` / `message::WsEvent`）；
//! Episode 化与 run 挂接（含认证 5s / 心跳 60s 时序、断线重连、场次窗）是第二段的活，
//! 本模块不抢跑、不碰既有黄金样本 / parity 面。
//!
//! 两个文件的分工（体积纪律：单文件 >500 行必分）：
//! - `codec`：16 字节大端头切包/组装、版本 2/3 解压多包展开（字节层）。
//! - `message`：op=5 行下 JSON → 最小消息族事件、op=3/op=8 回复、认证/心跳构造器。

pub mod codec;
pub mod message;
