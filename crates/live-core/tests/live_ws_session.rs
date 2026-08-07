//! 「WS 弹幕窗」—— 第二段 2A：会话录制引擎的验收钉（测试即规格）。
//!
//! 时序规格来源=执行单批 2（`docs/2026-08-06-r2-execution-spec.md` 批 2
//! 「WS 弹幕窗」第二段 2A「WS 会话录制引擎」）。钉脚以毫秒级合成时序驱动：
//! 认证窗口 300ms · 心跳间隔 50ms · 判死 300ms · 退避 [50 80 120]ms · 保险丝 200ms。
//!
//! 时序单位裁决（与 `session.rs` 模块头注一致）：`WsSessionConfig` 里所有 `*_secs`
//! 字段**按毫秒解释**；测试层直接写字面毫秒，生产默认走 `DEFAULT_*_MS` 系列常量。
//!
//! mock 方式：每钉起一条本机 TCP + `accept_async`（真实 WebSocket 握手），客户端
//! （被测方）用 `run_session` 真连真收。服务端是「一连接一剧本」的顺序消费者：
//! `Vec<Script>` 每次 accept 消耗一条；剧本耗尽即 task 结束（listener 回收），
//! 此后客户端若再多连会被拒、走退避（正好命中 ReconnectExhausted 语义）。
//! 剧本内的断言 panic 经 JoinHandle 重新抛到测试线程（见 `finish_mock`），不静默吞错。
//!
//! 合规红线（§11）专题钉：token 绝不进错误串/日志。

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures_util::{SinkExt, StreamExt};
use live_core::live_ws::codec::{RawPacket, decode_packets, encode_packet};
use live_core::live_ws::message::WsEvent;
use live_core::live_ws::session::{SessionEnd, SessionReport, WsSessionConfig, run_session};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, Instant, timeout};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::Message;

type Ws = WebSocketStream<TcpStream>;
type BoxFut = Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>;
/// 一条连接的服务器剧本。
type Script = Box<dyn Fn(Ws) -> BoxFut + Send + 'static>;

fn scr(f: impl Fn(Ws) -> BoxFut + Send + 'static) -> Script {
    Box::new(f)
}

// ---------------------------------------------------------------------------
// mock 服务器：一连接一剧本，剧本耗尽 listener 即回收
// ---------------------------------------------------------------------------

fn spawn_mock(listener: TcpListener, scripts: Vec<Script>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for script in scripts {
            let Ok((tcp, _addr)) = listener.accept().await else {
                break;
            };
            let Ok(ws) = accept_async(tcp).await else {
                continue;
            };
            script(ws).await;
        }
    })
}

async fn finish_mock(server: tokio::task::JoinHandle<()>) {
    match timeout(Duration::from_secs(8), server).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("mock 服务器剧本 panic：{e:?}"),
        Err(_) => panic!("mock 服务器 8s 未收尾"),
    }
}

/// 绑定本机随机端口，返回 (listener, ws url)。
async fn bind_ws() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind 本机");
    let port = listener.local_addr().expect("addr").port();
    (listener, format!("ws://127.0.0.1:{port}/sub"))
}

const TEST_TOKEN: &str = "pin-token-2-secret";
const ROOM_ID: i64 = 21_452_537;

// ---------------------------------------------------------------------------
// 配置与收集
// ---------------------------------------------------------------------------

/// 测试 base 时序（全毫秒）：认证 300 / 心跳 50 / 判死 300 / 退避 [50 80 120] / 保险丝 30s。
fn base_cfg(url: &str) -> WsSessionConfig {
    WsSessionConfig {
        url: url.to_string(),
        room_id: ROOM_ID,
        uid: 7,
        token: TEST_TOKEN.to_string(),
        cookie: String::new(),
        auth_deadline_ms: 300,
        heartbeat_interval_ms: 50,
        heartbeat_timeout_ms: 300,
        reconnect_backoff: vec![50, 80, 120],
        failsafe_cap_ms: 30_000,
    }
}

/// 跑 run_session 并收集事件；15s 硬超时改 panic。
async fn run_collect(cfg: &WsSessionConfig) -> (SessionReport, Vec<WsEvent>) {
    let mut events: Vec<WsEvent> = Vec::new();
    let mut emit = |ev: &WsEvent| {
        events.push(ev.clone());
        Ok::<(), String>(())
    };
    let report = timeout(
        Duration::from_secs(15),
        run_session(cfg, &mut emit, &|| 1_752_000_000),
    )
    .await
    .expect("run_session 挂死（15s 兜底）")
    .expect("run_session 不应在这批钉报致命错误");
    (report, events)
}

fn danmaku_texts(events: &[WsEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            WsEvent::Danmaku { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 帧构造（与 codec/message 同一套线格式）
// ---------------------------------------------------------------------------

fn p(op: u32, version: u16, body: &[u8]) -> Vec<u8> {
    encode_packet(op, version, body)
}

fn auth_ok() -> Vec<u8> {
    p(8, 0, br#"{"code":0}"#)
}

fn auth_refused() -> Vec<u8> {
    p(8, 0, br#"{"code":-101}"#)
}

fn danmaku(text: &str, uid: i64, uname: &str) -> Vec<u8> {
    let body = json!({
        "cmd": "DANMU_MSG",
        "info": [0, text, [uid, uname]],
    });
    p(5, 0, body.to_string().as_bytes())
}

fn popularity(v: u32) -> Vec<u8> {
    p(3, 1, &v.to_be_bytes())
}

fn live(live_time: i64) -> Vec<u8> {
    let body = json!({"cmd": "LIVE", "live_time": live_time});
    p(5, 0, body.to_string().as_bytes())
}

fn preparing(round: i64) -> Vec<u8> {
    let body = json!({"cmd": "PREPARING", "round": round});
    p(5, 0, body.to_string().as_bytes())
}

/// 版本 2 = zlib：body 是内层多包串联流。
fn zlib_outer(frames: &[Vec<u8>]) -> Vec<u8> {
    use std::io::Write;
    let mut inner = Vec::new();
    for f in frames {
        inner.extend_from_slice(f);
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&inner).expect("zlib write");
    p(5, 2, &enc.finish().expect("zlib finish"))
}

/// 版本 3 = brotli：外衣是压缩的多包串联流。
fn brotli_outer(frames: &[Vec<u8>]) -> Vec<u8> {
    use std::io::Write;
    let mut inner = Vec::new();
    for f in frames {
        inner.extend_from_slice(f);
    }
    let mut enc = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
    enc.write_all(&inner).expect("brotli write");
    p(5, 3, &enc.into_inner())
}

fn unpack_client_frame(msg: &Message) -> Vec<RawPacket> {
    match msg {
        Message::Binary(d) => decode_packets(d).expect("客户端帧可切包"),
        _ => panic!("客户端应发 Binary，收到 {msg:?}"),
    }
}

/// 服务端读客户端首帧并断言是认证帧（op=7），返回认证包体。
async fn expect_auth(ws: &mut Ws) -> Vec<u8> {
    let m = ws
        .next()
        .await
        .expect("服务端应收到客户端首帧")
        .expect("线读失败");
    let pkts = unpack_client_frame(&m);
    let auth = pkts
        .iter()
        .find(|p| p.op == 7)
        .expect("客户端首帧应为 op=7 认证");
    auth.body.clone()
}

/// 服务端视角统计一条帧里的 op=2 心跳包数（Ping/Pong/Close 不计数）。
fn op2_count(msg: &Message) -> usize {
    match msg {
        Message::Binary(d) => decode_packets(d)
            .map(|pkts| pkts.iter().filter(|p| p.op == 2).count())
            .unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// 钉 1：认证 —— op=7 + body 四件套在窗口内送达；拒绝 → AuthFailed；拖延 → Transport
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin1_auth_op7_arrives_within_window() {
    let (listener, url) = bind_ws().await;
    let auth_seen = Arc::new(AtomicBool::new(false));
    let flag = auth_seen.clone();

    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            let flag = flag.clone();
            Box::pin(async move {
                // 逐条复核认证 body（协议四要素）
                let body: Value =
                    serde_json::from_slice(&expect_auth(&mut ws).await).expect("认证 body 为 JSON");
                assert_eq!(body["uid"], json!(7), "认证带 uid");
                assert_eq!(body["roomid"], json!(ROOM_ID), "认证带 roomid");
                assert_eq!(body["protover"], json!(3), "认证声明 protover=3");
                assert_eq!(body["key"], json!(TEST_TOKEN), "认证带 key=token");
                flag.store(true, Ordering::SeqCst);

                ws.send(Message::Binary(auth_ok())).await.unwrap();
                ws.send(Message::Binary(preparing(1))).await.unwrap();
                let _ = ws.next().await; // 收对端 Close
            })
        })],
    );

    let cfg = base_cfg(&url);
    let (report, _events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert!(auth_seen.load(Ordering::SeqCst), "服务端必须收到合规认证帧");
    assert_eq!(report.end, SessionEnd::Closed, "PRE 后应正常关窗");
}

#[tokio::test]
async fn live_ws_pin_auth_refused_reports_auth_failed() {
    let (listener, url) = bind_ws().await;
    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_refused())).await.unwrap();
                let _ = ws.next().await; // 等对端 Close
            })
        })],
    );

    let cfg = base_cfg(&url);
    let (report, _events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert!(
        matches!(report.end, SessionEnd::AuthFailed { code: -101 }),
        "认证拒绝应报 AuthFailed{{code:-101}}，得到 {:?}",
        report.end
    );
}

#[tokio::test]
async fn live_ws_pin_auth_timeout_reports_transport_no_token_leak() {
    let (listener, url) = bind_ws().await;
    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                // 故意不回 op=8：让认证窗口（300ms）跑满 → Transport
                tokio::time::sleep(Duration::from_millis(600)).await;
                let _ = ws.flush().await;
            })
        })],
    );

    let cfg = base_cfg(&url);
    let (report, _events) = run_collect(&cfg).await;
    finish_mock(server).await;
    let text = match &report.end {
        SessionEnd::Transport(t) => t,
        other => panic!("认证超时应 Transport，得到 {other:?}"),
    };
    assert!(
        !text.contains(TEST_TOKEN),
        "Transport 文本不得泄露 token（合规红线）：{text}"
    );
}

// ---------------------------------------------------------------------------
// 钉 2：心跳 —— 服务端收到 ≥2 个 op=2；客户端收到 op=3 → Popularity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin_heartbeat_flow_and_popularity() {
    let (listener, url) = bind_ws().await;
    let hb_seen = Arc::new(AtomicUsize::new(0));
    let seen = hb_seen.clone();

    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            let seen = seen.clone();
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();

                // 收满 ≥2 个 op=2（间隔 50ms）
                let mut got = 0usize;
                while got < 2 {
                    let m = ws.next().await.expect("等客户端心跳").expect("线读失败");
                    got += op2_count(&m);
                }
                seen.store(got, Ordering::SeqCst);

                ws.send(Message::Binary(popularity(98_765))).await.unwrap();
                ws.send(Message::Binary(preparing(1))).await.unwrap();
                let _ = ws.next().await;
            })
        })],
    );

    let cfg = base_cfg(&url);
    let (report, events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert!(
        hb_seen.load(Ordering::SeqCst) >= 2,
        "服务端必须收到 ≥2 个 op=2 心跳"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WsEvent::Popularity { value: 98_765 })),
        "客户端必须产出 Popularity{{98765}}"
    );
    assert_eq!(report.end, SessionEnd::Closed);
}

// ---------------------------------------------------------------------------
// 钉 3：退避重连 —— 每连数据不丢、四连后预算尽 → ReconnectExhausted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin_backoff_reconnect_preserves_order() {
    let (listener, url) = bind_ws().await;

    // 1 首连 + 3 退避 = 4 个剧本；每个连接授权后给一条弹幕并断线
    let scripts: Vec<Script> = (0..4)
        .map(|line| {
            scr(move |mut ws| {
                Box::pin(async move {
                    expect_auth(&mut ws).await;
                    ws.send(Message::Binary(auth_ok())).await.unwrap();
                    ws.send(Message::Binary(danmaku(&format!("line-{line}"), 1, "u")))
                        .await
                        .unwrap();
                    let _ = ws.flush().await;
                    // drop → 客户端读到 None → Retry → 退避
                })
            })
        })
        .collect();
    let server = spawn_mock(listener, scripts);

    let cfg = base_cfg(&url);
    let (report, events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert_eq!(report.end, SessionEnd::ReconnectExhausted);
    assert_eq!(report.reconnects_used, 3, "四连中三次是重连");
    assert_eq!(
        danmaku_texts(&events),
        vec!["line-0", "line-1", "line-2", "line-3"],
        "断线重连不丢弹幕且保序"
    );
}

// ---------------------------------------------------------------------------
// 钉 4：PREPARING 关窗 —— 收 PRE→Closed，且之后服务端不再见新 op=2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin_preparing_round1_closes_no_more_heartbeats() {
    let (listener, url) = bind_ws().await;
    let hb_after_pre = Arc::new(AtomicUsize::new(9_999));
    let after = hb_after_pre.clone();

    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            let after = after.clone();
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                ws.send(Message::Binary(preparing(1))).await.unwrap();

                // 关窗后在观察窗内只应收到 Close
                let mut n = 0usize;
                while let Some(Ok(m)) = ws.next().await {
                    n += op2_count(&m);
                }
                after.store(n, Ordering::SeqCst);
            })
        })],
    );

    let cfg = base_cfg(&url);
    let (report, events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert_eq!(report.end, SessionEnd::Closed);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WsEvent::Preparing { round: 1 })),
        "必须有 Preparing{{round:1}}"
    );
    assert_eq!(
        hb_after_pre.load(Ordering::SeqCst),
        0,
        "PRE 后不得再收到 op=2 心跳"
    );
}

// ---------------------------------------------------------------------------
// 钉 5：保险丝 —— 200ms 静默连接 → TimedOut
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin_failsafe_200ms_quiet_times_out() {
    let (listener, url) = bind_ws().await;
    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                // 认证后彻底静默，保持连接存活；保险丝 200ms 应触顶
                tokio::time::sleep(Duration::from_millis(700)).await;
                let _ = ws.flush().await;
            })
        })],
    );

    let mut cfg = base_cfg(&url);
    cfg.failsafe_cap_ms = 200; // 保险丝 200ms
    cfg.heartbeat_timeout_ms = 30_000; // 排除心跳判死干扰，只留保险丝
    cfg.reconnect_backoff.clear(); // 会话到点即终，不再重连试探

    let start = Instant::now();
    let (report, _events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert!(
        matches!(report.end, SessionEnd::TimedOut),
        "静默连接必须 TimedOut，得到 {:?}",
        report.end
    );
    let el = start.elapsed();
    assert!(
        el >= Duration::from_millis(195) && el < Duration::from_secs(2),
        "保险丝应在 ~200ms 触顶，实际 {el:?}"
    );
}

// ---------------------------------------------------------------------------
// 钉 6：同窗续接 —— 两条 zlib 连接间断了、第三条收束：事件保序且不造新会话
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin_zlib_continuation_across_reconnects() {
    let (listener, url) = bind_ws().await;

    let scripts: Vec<Script> = vec![
        // A：zlib 双包（人气 + 弹幕 A1）
        scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                ws.send(Message::Binary(zlib_outer(&[
                    popularity(10),
                    danmaku("A1", 1, "a"),
                ])))
                .await
                .unwrap();
                let _ = ws.flush().await; // 断线 → Retry
            })
        }),
        // B：zlib 弹幕 B2
        scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                ws.send(Message::Binary(zlib_outer(&[danmaku("B2", 2, "b")])))
                    .await
                    .unwrap();
                let _ = ws.flush().await;
            })
        }),
        // C：zlib 弹幕 C3 + PRE 收窗
        scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                ws.send(Message::Binary(zlib_outer(&[
                    danmaku("C3", 3, "c"),
                    popularity(20),
                ])))
                .await
                .unwrap();
                ws.send(Message::Binary(preparing(1))).await.unwrap();
                let _ = ws.next().await;
            })
        }),
    ];

    let server = spawn_mock(listener, scripts);
    let cfg = base_cfg(&url);
    let (report, events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert_eq!(
        danmaku_texts(&events),
        vec!["A1", "B2", "C3"],
        "跨断线 zlib 多包流保序、点不丢（同一 run_session 会话）"
    );
    assert_eq!(report.reconnects_used, 2, "两度断线各自触发重连");
    assert_eq!(report.end, SessionEnd::Closed, "末窗 PRE 后 Closed");
}

// ---------------------------------------------------------------------------
// 钉 7：LIVE 槽位 —— 收到 LIVE{live_time} → report.last_live_time
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin_live_time_reported() {
    let (listener, url) = bind_ws().await;
    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                ws.send(Message::Binary(live(1_722_000_000))).await.unwrap();
                ws.send(Message::Binary(preparing(1))).await.unwrap();
                let _ = ws.next().await;
            })
        })],
    );

    let cfg = base_cfg(&url);
    let (report, events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            WsEvent::Live {
                live_time: 1_722_000_000
            }
        )),
        "必须产出 LIVE 事件"
    );
    assert_eq!(
        report.last_live_time,
        Some(1_722_000_000),
        "LIVE 槽位必须落 report"
    );
}

// ---------------------------------------------------------------------------
// 钉 8：自定义编解码 —— brotli 外衣可正常展开、内层事件照常
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ws_pin_brotli_envelope_expands() {
    let (listener, url) = bind_ws().await;
    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                // brotli 外衣里包弹幕 + 人气
                ws.send(Message::Binary(brotli_outer(&[
                    danmaku("brotli-msg", 9, "cd"),
                    popularity(77),
                ])))
                .await
                .unwrap();
                ws.send(Message::Binary(preparing(1))).await.unwrap();
                let _ = ws.next().await;
            })
        })],
    );

    let cfg = base_cfg(&url);
    let (report, events) = run_collect(&cfg).await;
    finish_mock(server).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WsEvent::Danmaku { text, .. } if text == "brotli-msg")),
        "brotli 弹幕必须解出，实际 {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WsEvent::Popularity { value: 77 })),
        "brotli 人气必须解出"
    );
    assert_eq!(report.end, SessionEnd::Closed);
}
