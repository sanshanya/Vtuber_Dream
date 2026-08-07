//! 「WS 弹幕窗」—— 第二段 2A：WS 会话录制引擎（会话状态机 + 事件输出回调）。
//!
//! 时序规格来源=执行单（`docs/2026-08-06-r2-execution-spec.md`「WS 弹幕窗」，
//! 第二段 2A「会话录制引擎」逐条工程化）。字节/事件面全部复用本模块第一段 codec/message，
//! 本段只做会话语义：认证 5s · 心跳 30s/60s 判死 · 断线指数退避重连 · 同窗续接不造新场次 ·
//! PREPARING 关窗 · 12h 保险丝（WS_FAILSAFE_CAP_HOURS）。
//!
//! 本层无 HTTP（getDanmuInfo / run / registry / Episode 化是下一段 2B 的活）：
//! url/token/roomid/uid 全由调用方注入；上层判「致命」即让事件回调返回 `Err`，
//! 本层立即终止会话并把消息原样返给调用方。
//!
//! 时序单位一以贯之：全部时序字段与生产默认常量都以**毫秒**存（字段名带 `_ms`
//! 后缀，与数值单位同名）；规格书写的秒值（5s/30s/60s/退避台阶/12h）在默认常量处
//! 对应换算到位（5s→5000、30s→30000、60s→60000、12h→43_200_000、台阶 ×1000）。
//!
//! 合规红线（§11）：token/cookie 绝不进任何错误串/日志——出界错误文本一律过 `scrub_text`
//! （替换 token/cookie/url，截 200 字符）。本层也不写任何日志。

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::{Instant, sleep, sleep_until};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request, header};
use tokio_tungstenite::tungstenite::protocol::Message;

use super::codec::{OP_DOWNSTREAM, WS_FAILSAFE_CAP_HOURS, decode_packets, encode_packet, unpack};
use super::message::{IgnoreTally, WsEvent, auth_packet_body, heartbeat_frame, parse_packet};

// ---------------------------------------------------------------------------
// 生产默认常量（单位：毫秒；数值 = 规格「秒」× 1000）
// ---------------------------------------------------------------------------

/// 认证确认窗口：连上后 5s（协议硬约束）内必须收到 op=8。
pub const DEFAULT_AUTH_DEADLINE_MS: u64 = 5_000;
/// 心跳发送间隔：30s。
pub const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 30_000;
/// 心跳判死窗口：60s 内未收到任何包即判死、进重连。
pub const DEFAULT_HEARTBEAT_TIMEOUT_MS: u64 = 60_000;
/// 断线指数退避台阶：1s/2s/4s/8s/16s/32s；耗尽 = 重连预算尽。
pub const DEFAULT_RECONNECT_BACKOFF_MS: [u64; 6] = [1_000, 2_000, 4_000, 8_000, 16_000, 32_000];
/// 会话保险丝：12h（= WS_FAILSAFE_CAP_HOURS × 3600s × 1000ms）。
pub const DEFAULT_FAILSAFE_CAP_MS: u64 = (WS_FAILSAFE_CAP_HOURS as u64) * 3_600_000;

/// 会话时序与连接参数（连接参数只收调用方值，本层不自行取参数）。
///
/// 时序字段**全部按毫秒**解释（裁决见模块头注）；生产默认走 `WsSessionConfig::new`。
#[derive(Debug, Clone)]
pub struct WsSessionConfig {
    /// `wss://{host}/sub`。
    pub url: String,
    /// 真实房间号。
    pub room_id: i64,
    /// 0 = 游客（默认）。
    pub uid: i64,
    /// getDanmuInfo 拿到的认证 key。
    pub token: String,
    /// 备用面 cookie（无则空串）。
    pub cookie: String,
    /// 认证确认窗口（毫秒；默认 5000 = 5s 协议硬约束）。
    pub auth_deadline_ms: u64,
    /// 心跳发送间隔（毫秒；默认 30000 = 30s）。
    pub heartbeat_interval_ms: u64,
    /// 心跳判死窗口（毫秒；默认 60000 = 60s，不收到任何包判死）。
    pub heartbeat_timeout_ms: u64,
    /// 断线退避台阶（毫秒；默认 [1000, 2000, …]）。耗尽 = 重连预算尽。
    pub reconnect_backoff: Vec<u64>,
    /// 会话保险丝（毫秒；默认 43_200_000 = 12h）。
    pub failsafe_cap_ms: u64,
}

impl WsSessionConfig {
    /// 以生产默认时序新建会话配置（连接参数由调用方给）。
    pub fn new(url: impl Into<String>, room_id: i64, token: impl Into<String>) -> Self {
        WsSessionConfig {
            url: url.into(),
            room_id,
            uid: 0,
            token: token.into(),
            cookie: String::new(),
            auth_deadline_ms: DEFAULT_AUTH_DEADLINE_MS,
            heartbeat_interval_ms: DEFAULT_HEARTBEAT_INTERVAL_MS,
            heartbeat_timeout_ms: DEFAULT_HEARTBEAT_TIMEOUT_MS,
            reconnect_backoff: DEFAULT_RECONNECT_BACKOFF_MS.to_vec(),
            failsafe_cap_ms: DEFAULT_FAILSAFE_CAP_MS,
        }
    }
}

/// 会话终点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// PREPARING 到 → 正常收拢（round 任意值，含轮播 1）。
    Closed,
    /// 保险丝（默认 12h 寿命）触顶。
    TimedOut,
    /// 断线退避预算耗尽且仍未看到 PREPARING（仍在播——本层不懂 HTTP，按尽预算终局）。
    ReconnectExhausted,
    /// 认证阶段 op=8 code != 0：直接终局、不重连。
    AuthFailed { code: i64 },
    /// ws 底层一次性错误文本（截 200 字）。本版里短时断线一律重连、最终报告落在
    /// ReconnectExhausted；`Transport` 只出在两类不可重连的终局：
    /// 认证确认窗口过期（未收到 op=8，配 pin 1）、以及上层完全不重连的接线。
    Transport(String),
}

/// 会话终局的报告（供 2B 段做场次窗校正与记账）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionReport {
    pub end: SessionEnd,
    pub events_emitted: u64,
    pub reconnects_used: u32,
    /// IgnoreTally 快照（BTreeMap 保序，复核/对账用）。
    pub ignores: BTreeMap<String, u64>,
    /// 本场期间收到 LIVE 的时刻；未收过 = None。
    pub last_live_time: Option<i64>,
    /// unix 秒。
    pub attached_at: i64,
    /// unix 秒。
    pub closed_at: i64,
}

/// 会话运行：连上 → 认证 → 心跳/判死/重连，直到 PREPARING（Closed）/认证失败/保险丝/预算尽。
///
/// - 每次解出一个事件回调一次；回调返回 `Err` 即断会话（上层判致命），本函数以 Err 返回、
///   不补齐 Report。
/// - 时钟注入：`now` 只用于报告里的 unix 秒锚点（attached/closed/last_live_time），
///   时序由毫秒参数 + tokio 单调时钟驱动（测试用短参数）。
pub async fn run_session(
    cfg: &WsSessionConfig,
    on_event: &mut (dyn FnMut(&WsEvent) -> Result<(), String> + Send),
    now: &dyn Fn() -> i64,
) -> Result<SessionReport, String> {
    let attached_at = now();
    let session_started = Instant::now();
    let request = build_client_request(cfg)?;

    let mut ctx = SessionCtx {
        cfg,
        on_event,
        session_started,
        request,
        tally: IgnoreTally::new(),
        events_emitted: 0,
        reconnects_used: 0,
        last_live_time: None,
        seen_preparing: false,
    };

    let failsafe_at = session_started + Duration::from_millis(cfg.failsafe_cap_ms);
    let mut backoff_steps_used = 0usize;
    let mut first_attempt = true;

    let end = loop {
        // PRE 已收 → 上一连接已把终局交了，不会走到这（防御）。
        if !first_attempt {
            if backoff_steps_used >= cfg.reconnect_backoff.len() {
                break SessionEnd::ReconnectExhausted;
            }
            let step_ms = cfg.reconnect_backoff[backoff_steps_used];
            backoff_steps_used += 1;
            ctx.reconnects_used += 1;
            if !sleep_interrupted_by_failsafe(Duration::from_millis(step_ms), failsafe_at).await {
                break SessionEnd::TimedOut;
            }
        }
        first_attempt = false;

        match run_one_connection(&mut ctx).await {
            Outcome::End(end) => break end,
            Outcome::Retry => continue,
            Outcome::Fatal(msg) => return Err(msg),
        }
    };

    Ok(SessionReport {
        end,
        events_emitted: ctx.events_emitted,
        reconnects_used: ctx.reconnects_used,
        ignores: ctx.tally.snapshot(),
        last_live_time: ctx.last_live_time,
        attached_at,
        closed_at: now(),
    })
}

// ---------------------------------------------------------------------------
// 会话内部状态与三个终局分支
// ---------------------------------------------------------------------------

/// 一条 `run_session` 的会话级状态；断线重连共享——事件/准备槽/人气槽跨连接累积，
/// 这正是「同窗续接不冒新场次」的低层含义（场次窗口由 2B 用 LIVE/PREPARING 管）。
struct SessionCtx<'a> {
    cfg: &'a WsSessionConfig,
    on_event: &'a mut (dyn FnMut(&WsEvent) -> Result<(), String> + Send),
    session_started: Instant,
    request: Request<()>,
    tally: IgnoreTally,
    events_emitted: u64,
    reconnects_used: u32,
    last_live_time: Option<i64>,
    seen_preparing: bool,
}

/// 一帧解析后的会话决策。
#[derive(Default)]
struct FrameDigest {
    auth_ok: bool,
    auth_failed: Option<i64>,
}

/// 连接层 → 会话层：终局 / 重试 / 致命三分。
enum Outcome {
    End(SessionEnd),
    Retry,
    Fatal(String),
}

// IgnoreTally 保留键（协议级改不改的记账口，绝不停机可见）。
const KEY_CODEC_ERR: &str = "sys.codec_error";
const KEY_NO_CMD_ERR: &str = "sys.no_cmd";

/// 装一帧字节：切包→解压→逐包解析→回调。坏包走 tally（包级容错，不炸会话）。
fn process_frame(ctx: &mut SessionCtx, data: &[u8]) -> Result<FrameDigest, String> {
    let mut digest = FrameDigest::default();
    let packets = match decode_packets(data) {
        Ok(pkts) => pkts,
        Err(_) => {
            ctx.tally.record(KEY_CODEC_ERR);
            return Ok(digest);
        }
    };
    for packet in packets {
        // 版本 2/3 自动展开内层多包流；解压失败按包级容错记账继续。
        let inners = match unpack(&packet) {
            Ok(inner) => inner,
            Err(_) => {
                ctx.tally.record(KEY_CODEC_ERR);
                continue;
            }
        };
        for inner in inners {
            match parse_packet(&inner) {
                Err(_) => ctx.tally.record(KEY_CODEC_ERR),
                Ok(None) => {
                    // op=5 的未知/缺段消息记 IgnoreTally（只登记不解析的可忽略量）。
                    if inner.op == OP_DOWNSTREAM {
                        match peek_cmd(&inner.body) {
                            Some(cmd) => ctx.tally.record(&cmd),
                            None => ctx.tally.record(KEY_NO_CMD_ERR),
                        }
                    }
                }
                Ok(Some(ev)) => {
                    ctx.events_emitted += 1;
                    (ctx.on_event)(&ev).map_err(|e| format!("事件回调终止会话：{e}"))?;
                    match &ev {
                        WsEvent::AuthAck { code } => {
                            if *code == 0 {
                                digest.auth_ok = true;
                            } else {
                                digest.auth_failed = Some(*code);
                            }
                        }
                        WsEvent::Preparing { .. } => ctx.seen_preparing = true,
                        WsEvent::Live { live_time } => ctx.last_live_time = Some(*live_time),
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(digest)
}

/// 取 op=5 下行 JSON 的 cmd 名（未知/已登记不解析的包进 IgnoreTally）。
fn peek_cmd(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("cmd")?.as_str().map(String::from)
}

/// 组装 WS 握手请求（cookie 只进 header，绝不出现在任何错误串）。
fn build_client_request(cfg: &WsSessionConfig) -> Result<Request<()>, String> {
    let mut req = cfg
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| scrub_text(cfg, &e.to_string()))?;
    if !cfg.cookie.is_empty() {
        let hdr = HeaderValue::from_str(cfg.cookie.as_str())
            .map_err(|e| format!("cookie 头无法构形：{e}"))?;
        req.headers_mut().insert(header::COOKIE, hdr);
    }
    Ok(req)
}

/// 退避睡眠，但被保险丝截断：返回 true=睡满退避台阶，false=保险丝提前触顶。
async fn sleep_interrupted_by_failsafe(wait: Duration, failsafe_at: Instant) -> bool {
    let now = Instant::now();
    if now >= failsafe_at {
        return false;
    }
    let remains = failsafe_at.saturating_duration_since(now);
    tokio::select! {
        _ = sleep(wait) => true,
        _ = sleep(remains) => false,
    }
}

/// 合规清洗：token/cookie/url 片段替换为占位、截 200 字符。
fn scrub_text(cfg: &WsSessionConfig, raw: &str) -> String {
    let mut s = raw.to_string();
    if !cfg.token.is_empty() {
        s = s.replace(cfg.token.as_str(), "…(token omitted)");
    }
    if !cfg.cookie.is_empty() {
        s = s.replace(cfg.cookie.as_str(), "…(cookie omitted)");
    }
    s = s.replace(cfg.url.as_str(), "…(url omitted)");
    s.chars().take(200).collect()
}

/// 一条连接的完整生命周期：握手 → 认证 → 运行（心跳/判死/保险丝）→ 终局/重试。
async fn run_one_connection(ctx: &mut SessionCtx<'_>) -> Outcome {
    let conn_config = ctx.cfg;
    let conn_started = Instant::now();
    let auth_deadline = Duration::from_millis(conn_config.auth_deadline_ms);
    let heartbeat_interval = Duration::from_millis(conn_config.heartbeat_interval_ms);
    let heartbeat_timeout = Duration::from_millis(conn_config.heartbeat_timeout_ms);
    let failsafe_at = ctx.session_started + Duration::from_millis(conn_config.failsafe_cap_ms);

    let mut ws = match connect_async(ctx.request.clone()).await {
        Ok((ws, _)) => ws,
        // 握手失败：错误文本不外泄（§11）且属短时错误 → 交给外层退避重连。
        Err(_) => return Outcome::Retry,
    };

    // 认证帧：op=7 / version 0 / body = auth_packet_body（先发认证、再等 op=8）。
    let auth_body = auth_packet_body(conn_config.room_id, conn_config.uid, &conn_config.token);
    let auth_frame = encode_packet(7, 0, &auth_body);
    if ws.send(Message::Binary(auth_frame)).await.is_err() {
        return Outcome::Retry;
    }

    let mut last_seen = Instant::now();
    let mut last_hb = Instant::now();
    let mut authenticated = false;

    loop {
        let auth_at = conn_started + auth_deadline;
        let hb_at = last_hb + heartbeat_interval;
        let seen_at = last_seen + heartbeat_timeout;

        // 控制臂：认证窗口 / 心跳判死窗 / 保险丝，三合一取最先到点。
        let ctl = async {
            if authenticated {
                sleep_until(seen_at.min(failsafe_at)).await;
            } else {
                sleep_until(auth_at.min(failsafe_at)).await;
            }
        };
        // 心跳臂：认证通过前不参与选择。
        let hb = async {
            if authenticated {
                sleep_until(hb_at).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            msg = ws.next() => {
                match msg {
                    None => return Outcome::Retry,
                    Some(Err(_)) => return Outcome::Retry,
                    Some(Ok(Message::Binary(data))) => {
                        last_seen = Instant::now();
                        match process_frame(ctx, &data) {
                            Err(msg) => return Outcome::Fatal(msg),
                            Ok(digest) => {
                                if ctx.seen_preparing {
                                    return Outcome::End(SessionEnd::Closed);
                                }
                                if let Some(code) = digest.auth_failed {
                                    return Outcome::End(SessionEnd::AuthFailed { code });
                                }
                                if !authenticated && digest.auth_ok {
                                    authenticated = true;
                                    last_hb = Instant::now();
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Text(txt))) => {
                        last_seen = Instant::now();
                        match process_frame(ctx, txt.as_bytes()) {
                            Err(msg) => return Outcome::Fatal(msg),
                            Ok(digest) => {
                                if ctx.seen_preparing {
                                    return Outcome::End(SessionEnd::Closed);
                                }
                                if let Some(code) = digest.auth_failed {
                                    return Outcome::End(SessionEnd::AuthFailed { code });
                                }
                                if !authenticated && digest.auth_ok {
                                    authenticated = true;
                                    last_hb = Instant::now();
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(pong))) => {
                        last_seen = Instant::now();
                        if ws.send(Message::Pong(pong)).await.is_err() {
                            return Outcome::Retry;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => last_seen = Instant::now(),
                    Some(Ok(Message::Close(_))) => {
                        // 对端关连：窗口未关则走重连；已关（PREPARING）不会到这一步。
                        return if ctx.seen_preparing {
                            Outcome::End(SessionEnd::Closed)
                        } else {
                            Outcome::Retry
                        };
                    }
                    Some(Ok(Message::Frame(_))) => {}
                }
            }
            _ = ctl => {
                let now = Instant::now();
                if now >= failsafe_at {
                    return Outcome::End(SessionEnd::TimedOut);
                }
                if authenticated {
                    return Outcome::Retry; // 心跳判死：窗口期无任何包
                }
                // 认证 5s 未确认：不重连（重连只用于已认证后的断线），直接终局。
                // 消息不带任何 token 内容（§11 合规红线），token 校验细节留给 2B 层。
                return Outcome::End(SessionEnd::Transport(
                    "认证确认超时：(连接后指定窗口内未收到 op=8)".to_string(),
                ));
            }
            _ = hb => {
                if ws.send(Message::Binary(heartbeat_frame())).await.is_err() {
                    return Outcome::Retry;
                }
                last_hb = Instant::now();
            }
        }
    }
}
