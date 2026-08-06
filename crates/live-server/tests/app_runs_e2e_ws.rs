//! 轮2-R2-2B（D1「WS 弹幕窗」挂接）e2e：collect_streamer 尾段的
//! `live_ws_record` 弹幕窗采录全链路验收。
//!
//! 四钉：
//! 1. `live_ws_record: 1` + 房间在播（get_info live_status=1）+ 本机 WS mock
//!    （auth_ok → 2 弹幕 → PREPARING）→ 弹幕窗收窗（end_reason=preparing），
//!    outcome.ws_window 在位、_room 语料入账 graph（source=live_ws_danmaku）；
//! 2. `live_ws_record` 缺省（0）→ 不开窗，无 ws_window 键，
//!    events 含「[WS] 本轮未开弹幕窗（配置关/房间未在播）」；
//! 3. `live_ws_record: 1` 但房间未在播（get_info live_status=0）→ 不开窗，
//!    events 含「[WS] 房间未在播」；
//! 4. 认证拒绝（op=8 code=-101 → AuthFailed）→ 诚实窗 end_reason=auth_failed，
//!    ws_window 仍在 outcome、run 照常 done、token 绝不进 events。
//!
//! mock 分工：wiremock 只答 HTTP（get_info / getDanmuInfo / 采集基线），
//! 弹幕网关 = 本机 TCP + `accept_async`（真实 WebSocket 握手，与
//! `live-core/tests/live_ws_session.rs` 同源）。`DanmakuInfo::url()` 对非 443
//! 端口出 `ws://host:port/sub`，故 listener 必须先绑定拿端口、再挂 getDanmuInfo
//! （wiremock 模板静态，端口要在 mount 前就定）。

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use axum::http::Request;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::Message;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use live_core::live_ws::codec::{RawPacket, decode_packets, encode_packet};
use live_server::app::{AppState, build_app};
use live_server::registry::Registry;

mod common;

// ---------------------------------------------------------------------------
// wiremock 布景：采集基线 + D1 两个新端点（get_info / getDanmuInfo）
// ---------------------------------------------------------------------------

fn json_ok(data: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"code": 0, "message": "0", "data": data}))
}

async fn mount_bilibili(server: &MockServer, expect_path: &str, template: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(expect_path))
        .respond_with(template)
        .mount(server)
        .await;
}

/// collect_streamer 的采集基线（与 app_runs_e2e.rs 的 mount_bilibili_baseline 同源）。
async fn mount_bilibili_baseline(server: &MockServer) {
    mount_bilibili(
        server,
        "/x/web-interface/nav",
        json_ok(json!({
            "isLogin": true, "mid": 42, "uname": "me",
            "wbi_img": {
                "img_url": "https://i0.hdslb.com/bfs/wbi/abc123.png",
                "sub_url": "https://i0.hdslb.com/bfs/wbi/def456.png"
            }
        })),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/app-room/v2/guardTab/topListNew",
        json_ok(json!({"top3": [], "list": []})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/relation/stat",
        json_ok(json!({"following": 5, "follower": 9})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/wbi/acc/info",
        json_ok(json!({"name": "昵称", "face": "face", "sign": "签名", "level": 5})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/relation/followings",
        json_ok(json!({"list": [{"mid": 2001, "uname": "关注1", "sign": "s"}]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/wbi/arc/search",
        json_ok(json!({"list": {"vlist": [
            {"aid": 80433022, "bvid": "BV1xx", "title": "投稿1", "description": "d", "created": 1700000000}
        ]}})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/polymer/web-dynamic/v1/feed/space",
        json_ok(json!({"items": [{
            "id_str": "998",
            "modules": {
                "module_author": {"pub_ts": 1700001000},
                "module_dynamic": {"desc": {"text": "动态文本"}}
            }
        }]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/v3/fav/folder/created/list-all",
        json_ok(json!({"list": [{"id": 555, "title": "默认收藏夹", "media_count": 2, "attr": 0}]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/v3/fav/resource/list",
        json_ok(json!({
            "medias": [{
                "bvid": "BV2yy", "title": "收藏1", "intro": "i", "fav_time": 1700002000,
                "upper": {"mid": 3001, "name": "up1"}
            }],
            "has_more": false
        })),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/bangumi/follow/list",
        ResponseTemplate::new(404),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/lastplaygame/v2",
        json_ok(json!({"list": [{"name": "游戏A", "game_id": 7, "summary": "好玩"}]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/web-interface/view",
        json_ok(json!({
            "tid": 167, "tname": "知识", "parent_tid": 36, "tname_v2": "科普",
            "title": "T", "pubdate": 1700000000, "owner": {"mid": 3001, "name": "up1"}
        })),
    )
    .await;
    mount_bilibili(
        server,
        "/x/tag/archive/tags",
        json_ok(json!([{"tag_name": "tagA"}, {"name": "tagB"}])),
    )
    .await;
    mount_bilibili(
        server,
        "/x/web-interface/wbi/search/square",
        json_ok(json!({"trending": {"list": [{"keyword": "热词1"}]}})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/v2/reply",
        json_ok(json!({"replies": [{
            "rpid_str": "77",
            "member": {"mid": "8877", "uname": "路人甲"},
            "content": {"message": "前排"},
            "like": 5, "ctime": 1700003000, "rcount": 1
        }]})),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/web-room/v1/record/getList",
        json_ok(json!({"count": 2, "list": [
            {"rid": "R1Ex", "title": "回放A", "area_name": "虚拟主播", "parent_area_name": "娱乐",
             "start_timestamp": 1700000000, "end_timestamp": 1700000200, "danmu_num": 2, "length": 120}
        ]})),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/web-room/v1/record/getInfoByLiveRecord",
        json_ok(
            json!({"live_record_info": {"rid": "R1Ex"}, "dm_info": {"num": 1, "total_num": 2}}),
        ),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/web-room/v1/dM/getDMMsgByPlayBackID",
        json_ok(json!({"dm": {"dm_info": [
            {"text": "弹幕一", "uid": 998877, "medal": {"medal_name": "牌子", "medal_level": 7}},
            {"text": "弹幕二", "uid": 998878, "medal": null}
        ]}})),
    )
    .await;
}

/// D1 新端点：房间在播状态（get_info）。
async fn mount_room_live_status(server: &MockServer, live_status: i64) {
    mount_bilibili(
        server,
        "/room/v1/Room/get_info",
        json_ok(json!({"live_status": live_status})),
    )
    .await;
}

/// D1 新端点：弹幕网关凭据（getDanmuInfo 的 host_list[0] + token）。
/// `ws_port` 必须是已绑定的本机 listener 端口（`DanmakuInfo::url()` 走 ws://）。
async fn mount_danmu_info(server: &MockServer, ws_port: u16) {
    mount_bilibili(
        server,
        "/xlive/web-room/v1/index/getDanmuInfo",
        json_ok(json!({
            "host_list": [{"host": "127.0.0.1", "port": ws_port}],
            "token": WS_TEST_TOKEN,
        })),
    )
    .await;
}

// ---------------------------------------------------------------------------
// 本机 WS mock（与 live-core/tests/live_ws_session.rs 同源：一连接一剧本）
// ---------------------------------------------------------------------------

const WS_TEST_TOKEN: &str = "e2e-ws-token-secret";

type Ws = WebSocketStream<TcpStream>;
type BoxFut = Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>;
type Script = Box<dyn Fn(Ws) -> BoxFut + Send + 'static>;

fn scr(f: impl Fn(Ws) -> BoxFut + Send + 'static) -> Script {
    Box::new(f)
}

async fn bind_ws() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind 本机");
    let port = listener.local_addr().expect("addr").port();
    (listener, port)
}

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

fn preparing(round: i64) -> Vec<u8> {
    let body = json!({"cmd": "PREPARING", "round": round});
    p(5, 0, body.to_string().as_bytes())
}

fn unpack_client_frame(msg: &Message) -> Vec<RawPacket> {
    match msg {
        Message::Binary(d) => decode_packets(d).expect("客户端帧可切包"),
        _ => panic!("客户端应发 Binary，收到 {msg:?}"),
    }
}

/// 服务端读客户端首帧并断言是认证帧（op=7）。
async fn expect_auth(ws: &mut Ws) {
    let m = ws
        .next()
        .await
        .expect("服务端应收到客户端首帧")
        .expect("线读失败");
    let pkts = unpack_client_frame(&m);
    assert!(
        pkts.iter().any(|packet| packet.op == 7),
        "客户端首帧应为 op=7 认证，收到 {pkts:?}"
    );
}

// ---------------------------------------------------------------------------
// 通用辅助：fixture / 轮询 / oneshot / run 终局
// ---------------------------------------------------------------------------

/// `live_ws_record=1` 由调用方决定是否注入（replacen 到 timeout_seconds 行后）。
fn build_ws_fixture(
    bilibili_uri: &str,
    live_ws_record: bool,
) -> (tempfile::TempDir, PathBuf, axum::Router, Registry) {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir: PathBuf = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut yaml = common::yaml_template(
        None,
        "m5d-ws",
        "SESSDATA=test",
        "test-key",
        "http://127.0.0.1:9/v1",
        "m5d-ws",
    )
    .replace(
        "OUTPUT_DIR",
        &out_dir.display().to_string().replace('\\', "/"),
    );
    if live_ws_record {
        yaml = yaml.replacen(
            "  timeout_seconds: 5",
            "  timeout_seconds: 5\n  live_ws_record: 1",
            1,
        );
    }
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    let registry = Registry::new();
    let app = build_app(AppState {
        config_path: config_path.clone(),
        web_root: tmp.path().join("no-dist"),
        registry: registry.clone(),
        demo: false,
        data_root: None,
        bilibili_hosts: Some((bilibili_uri.to_string(), bilibili_uri.to_string())),
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });
    (tmp, config_path, app, registry)
}

fn wait_until(deadline: Duration, mut probe: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if probe() {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

async fn oneshot(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body
        .map(|value| axum::body::Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap_or_else(axum::body::Body::empty);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn run_terminal(run_id: &str, registry: &Registry, timeout: Duration) -> Value {
    let record = registry.get(run_id).expect("run registered");
    let finished = wait_until(timeout, || {
        let r = record.lock().expect("record poisoned");
        r.status == "done" || r.status == "failed"
    });
    let snapshot = live_server::registry::run_to_json(&record.lock().expect("record poisoned"));
    assert!(finished, "run 未在 {:?} 内到终：{snapshot}", timeout);
    snapshot
}

/// events 足迹里所有文本行（run 记录的事件是字符串数组）。
fn event_lines(snapshot: &Value) -> Vec<String> {
    snapshot["events"]
        .as_array()
        .expect("events list")
        .iter()
        .filter_map(|row| row.as_str().map(str::to_string))
        .collect()
}

fn graph_store(out_dir: &Path) -> live_core::graph::Store {
    live_core::graph::Store::open(&out_dir.join("graph").join("perception.sqlite3")).unwrap()
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// 钉 1：live_ws_record=1 + 房间在播 → 弹幕窗收窗、_room 语料入账 graph
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ws_window_records_danmaku_and_lands_in_graph() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    mount_room_live_status(&bilibili, 1).await;

    // 先绑本机 WS 网关（拿到真实端口）再挂 getDanmuInfo（模板静态）。
    let (listener, ws_port) = bind_ws().await;
    mount_danmu_info(&bilibili, ws_port).await;
    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                ws.send(Message::Binary(auth_ok())).await.unwrap();
                ws.send(Message::Binary(danmaku("你好", 1001, "弹幕甲")))
                    .await
                    .unwrap();
                ws.send(Message::Binary(danmaku("好耶", 1002, "弹幕乙")))
                    .await
                    .unwrap();
                ws.send(Message::Binary(preparing(1))).await.unwrap();
                // 读到客户端 Close 后收尾（PREPARING 后客户端即关线）。
                while let Some(Ok(_)) = ws.next().await {}
            })
        })],
    );

    let (_tmp, config_path, app, registry) = build_ws_fixture(&bilibili.uri(), true);
    let out_dir = live_core::config::load_config(&config_path)
        .expect("config loads")
        .output_dir;

    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "collect_streamer"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(60));
    finish_mock(server).await;

    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert_eq!(snapshot["kind"], "collect_streamer");

    // outcome.ws_window：2 弹幕、preparing 收窗、session 形状。
    let ws_window = &snapshot["outcome"]["ws_window"];
    assert!(
        ws_window.is_object(),
        "ws_window 应在 outcome 里：{snapshot}"
    );
    assert_eq!(ws_window["lines"], json!(2), "{ws_window}");
    assert_eq!(ws_window["end_reason"], json!("preparing"), "{ws_window}");
    assert_eq!(ws_window["counts"]["danmaku"], json!(2), "{ws_window}");
    assert_eq!(ws_window["session"]["room_id"], json!("983"), "{ws_window}");
    let rid = ws_window["session"]["rid"].as_str().expect("rid");
    assert!(rid.starts_with("ws:"), "rid 前缀应恒为 ws:，得到 {rid}");
    assert_eq!(
        ws_window["session"]["window_start"],
        json!("attach"),
        "{ws_window}"
    );
    assert_eq!(ws_window["unknowns"].as_array().map(Vec::len), Some(0));

    // events 足迹：开窗、收窗、完成三连。
    let lines = event_lines(&snapshot);
    assert!(
        lines.iter().any(|l| l.contains("[WS] live_ws_record 开启")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("end_reason=preparing") && l.contains("线 2 条")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[WS] 弹幕窗采录完成（2 线入账，end_reason=preparing")),
        "{lines:?}"
    );

    // graph：ws-record run 里 _room 语料，source=live_ws_danmaku。
    let store = graph_store(&out_dir);
    let eps = live_core::graph::query::episodes(&store, "_room", None).expect("episodes 查询");
    assert!(
        eps.iter().any(|e| e["source"] == json!("live_ws_danmaku")),
        "_room 应落 live_ws_danmaku 语料：{eps:?}"
    );
    let fact = eps
        .iter()
        .find(|e| e["source"] == json!("live_ws_danmaku"))
        .unwrap();
    assert_eq!(
        fact["platform_facts"]["session"]["rid"],
        json!(rid),
        "窗内线应带最终场次窗 rid"
    );

    // 旧 naming「status=complete 已冻结」不破：WS 只在 summary 上增 ws_window 键。
    assert_eq!(
        read(&out_dir.join("collection.json"))["status"],
        json!("complete"),
        "collection 状态面不得被 WS 尾段改写"
    );
}

// ---------------------------------------------------------------------------
// 钉 2：live_ws_record 缺省（0）→ 不开窗、无 ws_window 键
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ws_record_off_skips_window_entirely() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    // 不挂 getDanmuInfo / 不起 WS mock：配置关时 record_ws_window 直接短路。

    let (_tmp, _config_path, app, registry) = build_ws_fixture(&bilibili.uri(), false);
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "collect_streamer"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(60));

    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert!(
        snapshot["outcome"].get("ws_window").is_none(),
        "live_ws_record=0 时不得出现 ws_window：{snapshot}"
    );
    let lines = event_lines(&snapshot);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[WS] 本轮未开弹幕窗（配置关/房间未在播）")),
        "{lines:?}"
    );
}

// ---------------------------------------------------------------------------
// 钉 3：live_ws_record=1 但房间未在播 → 不开窗、无 ws_window 键
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ws_record_room_not_live_skips_window() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    mount_room_live_status(&bilibili, 0).await;
    // 未在播 → getDanmuInfo 都不该被调：不挂、不起 WS mock。

    let (_tmp, _config_path, app, registry) = build_ws_fixture(&bilibili.uri(), true);
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "collect_streamer"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(60));

    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert!(
        snapshot["outcome"].get("ws_window").is_none(),
        "房间未在播时不得出现 ws_window：{snapshot}"
    );
    let lines = event_lines(&snapshot);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[WS] 房间未在播（live_status=0），跳过弹幕窗采录")),
        "{lines:?}"
    );
}

// ---------------------------------------------------------------------------
// 钉 4：认证拒绝 → 诚实窗 end_reason=auth_failed；run 照常 done；token 不进 events
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ws_record_auth_refused_is_honest_window_no_token_leak() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    mount_room_live_status(&bilibili, 1).await;

    let (listener, ws_port) = bind_ws().await;
    mount_danmu_info(&bilibili, ws_port).await;
    let server = spawn_mock(
        listener,
        vec![scr(move |mut ws| {
            Box::pin(async move {
                expect_auth(&mut ws).await;
                // AuthFailed{code:-101}：会话立即终局、不重连（session.rs 语义）。
                ws.send(Message::Binary(auth_refused())).await.unwrap();
                let _ = ws.flush().await;
            })
        })],
    );

    let (_tmp, _config_path, app, registry) = build_ws_fixture(&bilibili.uri(), true);
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "collect_streamer"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(60));
    finish_mock(server).await;

    assert_eq!(snapshot["status"], "done", "{snapshot}");
    let ws_window = &snapshot["outcome"]["ws_window"];
    assert!(
        ws_window.is_object(),
        "认证拒绝也是诚实窗，ws_window 应在 outcome：{snapshot}"
    );
    assert_eq!(ws_window["end_reason"], json!("auth_failed"), "{ws_window}");
    assert_eq!(ws_window["lines"], json!(0), "{ws_window}");
    assert_eq!(ws_window["unknowns"].as_array().map(Vec::len), Some(0));

    // §11 红线：token 绝不进事件足迹、不进 outcome。
    let all_text = event_lines(&snapshot).join("\n");
    assert!(
        !all_text.contains(WS_TEST_TOKEN),
        "token 不得出现在 events：{all_text}"
    );
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(
        !serialized.contains(WS_TEST_TOKEN),
        "token 不得出现在 outcome/run 面：{serialized}"
    );
}
