//! JSON 事件层：op=5 行下 → 最小消息族事件；op=3 → 人气值；op=8 → 认证回复。
//! 认证/心跳构造器也放本侧（字节帧的组装在 `codec::encode_packet`）。
//!
//! 协议真理源：bilibili-API-collect `docs/live/message_stream.md`。合成字节流 fixture
//! 即规格：全部验收钉在 `crates/live-core/tests/live_ws_codec.rs`。
//!
//! 纪律（哲学红线：程序负责事实，绝不造词造谣）：
//! - 消息族最小集之外、或最小界面缺关键段的，一律 `Ok(None)`——登记不解析，
//!   缺段不补数；生病 JSON 才 `Err(BadJson)`。
//! - `INTERACT_WORD_V2`（base64 protobuf）本轮登记不解析，可被 `IgnoreTally` 计数。
//! - 调用方必须先对压缩包（version 2/3）跑 `codec::unpack` 再逐 inner 包
//!   `parse_packet`；把压缩原体直接喂进来会得到 `BadJson`（不静默透传）。
//!
//! 消息族最小集（op=5，`{cmd, ...}`；未知 cmd 一律忽略并计数）：
//!   DANMU_MSG             info[1]=文本；info[2][0]=真实 mid；info[2][1]=uname；
//!                         缺 info[1] / info[2] / uid → 跳过（Ok(None)）
//!   SUPER_CHAT_MESSAGE    与 _JPN 同构：data.{message, price, user_info.{uid, uname}}
//!   SUPER_CHAT_MESSAGE_DELETE  data.ids（只登记撤销事件）
//!   INTERACT_WORD         data.{msg_type（1 进场/2 关注/3 分享）, uid, uname, timestamp}
//!                         kind 取值原样登记
//!   LIVE                  {cmd:"LIVE", live_time:<epoch 秒>} 开播信号
//!   PREPARING             {cmd:"PREPARING", round?: 1} 下播/轮播信号（round 缺省=0）

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::Value;

use super::codec::{
    CodecError, OP_AUTH_REPLY, OP_DOWNSTREAM, OP_HEARTBEAT_CLIENT, OP_HEARTBEAT_REPLY, RawPacket,
    VERSION_INTEGER_BODY, encode_packet,
};

/// 协议事件（结构化事实；Episode 化与 run 挂接在第二段，本段只产事件）。
#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    /// op=8 认证回复：body JSON `{"code": N}`。
    AuthAck { code: i64 },
    /// op=3 心跳回复：body 前 4 字节大端 u32 人气值（平台事实「在线数」）。
    Popularity { value: u32 },
    /// DANMU_MSG：真实 mid / uname / 文本。
    Danmaku {
        uid: String,
        uname: String,
        text: String,
    },
    /// SUPER_CHAT_MESSAGE（含 _JPN 同构）：金额（元）/ 发者真实 mid / uname / 文本。
    SuperChat {
        uid: String,
        uname: String,
        text: String,
        price: f64,
    },
    /// SUPER_CHAT_MESSAGE_DELETE：只登记撤销事件的 ids。
    SuperChatDelete { ids: Vec<i64> },
    /// INTERACT_WORD：kind ∈ {1 进场,2 关注,3 分享}，其他值原样登记。
    Interact {
        kind: u32,
        uid: String,
        uname: String,
        ts: i64,
    },
    /// LIVE：开播信号；live_time 供第二段校正场次窗起点。
    Live { live_time: i64 },
    /// PREPARING：下播/轮播信号；round=1 轮播按下播同等处理。
    Preparing { round: i64 },
}

/// op=5（已解压的 inner 包）→ 事件；op=3 → Popularity；op=8 → AuthAck；其余 op → None。
/// 未知 cmd / 缺关键段 → Ok(None)（登记不解析）；坏 JSON → Err(BadJson)。压缩包先 unpack。
pub fn parse_packet(packet: &RawPacket) -> Result<Option<WsEvent>, CodecError> {
    match packet.op {
        OP_HEARTBEAT_REPLY => popularity_body(&packet.body),
        OP_AUTH_REPLY => auth_ack_body(&packet.body),
        OP_DOWNSTREAM => downstream_body(&packet.body),
        _ => Ok(None),
    }
}

/// op=3：body 即 4 字节大端 u32 人气值；不足 4 字节 → TruncatedBody（体不满）。
fn popularity_body(body: &[u8]) -> Result<Option<WsEvent>, CodecError> {
    if body.len() < 4 {
        return Err(CodecError::TruncatedBody);
    }
    let value = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    Ok(Some(WsEvent::Popularity { value }))
}

/// op=8：body JSON `{"code":N}`；非合法 JSON → Err；缺 code 段 → None（不给造成功/失败）。
fn auth_ack_body(body: &[u8]) -> Result<Option<WsEvent>, CodecError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| CodecError::BadJson(e.to_string()))?;
    let code = v.get("code").and_then(Value::as_i64);
    Ok(code.map(|code| WsEvent::AuthAck { code }))
}

/// op=5：body 必须是合法 JSON；cmd 缺失/未知 → Ok(None)。
fn downstream_body(body: &[u8]) -> Result<Option<WsEvent>, CodecError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| CodecError::BadJson(e.to_string()))?;
    let cmd = match v.get("cmd").and_then(Value::as_str) {
        Some(cmd) => cmd,
        None => return Ok(None),
    };
    Ok(match cmd {
        "DANMU_MSG" => parse_danmaku(&v),
        "SUPER_CHAT_MESSAGE" | "SUPER_CHAT_MESSAGE_JPN" => parse_super_chat(&v),
        "SUPER_CHAT_MESSAGE_DELETE" => parse_super_chat_delete(&v),
        "INTERACT_WORD" => parse_interact_word(&v),
        "LIVE" => parse_live(&v),
        "PREPARING" => parse_preparing(&v),
        // 未知 cmd（含 INTERACT_WORD_V2）：登记不解析，由第二段量 IgnoreTally。
        _ => None,
    })
}

/// JSON 数字 → 十进制字符串（i64/u64 直取；不背 f64 非整数值——绝不造数）。
fn num_to_string(v: &Value) -> Option<String> {
    v.as_i64()
        .map(|n| n.to_string())
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}

/// JSON 数字 → i64（u64 范围放不下时放弃）。
fn num_to_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
}

fn parse_danmaku(v: &Value) -> Option<WsEvent> {
    let info = v.get("info")?.as_array()?;
    // 文本原样（trim 属修饰——幂等键/正文哈希要吃原文，事实层不动字）。
    let text = info.get(1)?.as_str()?.to_string();
    let user = info.get(2)?.as_array()?;
    let uid = num_to_string(user.first()?)?;
    let uname = user
        .get(1)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(WsEvent::Danmaku { uid, uname, text })
}

fn parse_super_chat(v: &Value) -> Option<WsEvent> {
    let data = v.get("data")?.as_object()?;
    let text = data.get("message")?.as_str()?.to_string();
    let price = data.get("price")?.as_f64()?;
    let user_info = data.get("user_info")?.as_object()?;
    let uid = num_to_string(user_info.get("uid")?)?;
    let uname = user_info
        .get("uname")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(WsEvent::SuperChat {
        uid,
        uname,
        text,
        price,
    })
}

fn parse_super_chat_delete(v: &Value) -> Option<WsEvent> {
    let data = v.get("data")?.as_object()?;
    let ids: Vec<i64> = data
        .get("ids")?
        .as_array()?
        .iter()
        .filter_map(num_to_i64)
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(WsEvent::SuperChatDelete { ids })
    }
}

fn parse_interact_word(v: &Value) -> Option<WsEvent> {
    let data = v.get("data")?.as_object()?;
    let kind = data.get("msg_type").and_then(Value::as_u64)? as u32;
    let uid = num_to_string(data.get("uid")?)?;
    let uname = data
        .get("uname")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let ts = num_to_i64(data.get("timestamp")?)?;
    Some(WsEvent::Interact {
        kind,
        uid,
        uname,
        ts,
    })
}

fn parse_live(v: &Value) -> Option<WsEvent> {
    let live_time = num_to_i64(v.get("live_time")?)?;
    Some(WsEvent::Live { live_time })
}

fn parse_preparing(v: &Value) -> Option<WsEvent> {
    let round = v.get("round").and_then(Value::as_i64).unwrap_or(0);
    Some(WsEvent::Preparing { round })
}

/// 认证包 body（op=7 的负载）：`{"uid","roomid","protover":3,"key"}`。
/// protover=3 是**请求内容**（向服务端声明可接收 brotli），与帧头版本字段无关——
/// 认证帧头用 version 0（明文 JSON）：`encode_packet(7, 0, auth_packet_body(...))`。
pub fn auth_packet_body(room_id: i64, uid: i64, key: &str) -> Vec<u8> {
    serde_json::json!({
        "uid": uid,
        "roomid": room_id,
        "protover": 3,
        "key": key,
    })
    .to_string()
    .into_bytes()
}

/// 客户端心跳帧：op=2、version=1，body 是字节串 `[object Object]`。
/// 帧总长 = 16 头 + 15 body = 31 字节。
pub fn heartbeat_frame() -> Vec<u8> {
    encode_packet(
        OP_HEARTBEAT_CLIENT,
        VERSION_INTEGER_BODY,
        b"[object Object]",
    )
}

/// 协议级忽略计数器：未知 cmd / 只登记不解析的消息族在这里显形，绝不静默。
/// 本段只给出类型与 record/snapshot 口子；挂到会话上逐包记账是第二段的活。
#[derive(Debug, Default)]
pub struct IgnoreTally {
    inner: Mutex<BTreeMap<String, u64>>,
}

impl IgnoreTally {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, cmd: &str) {
        let mut map = self.inner.lock().expect("ignore tally lock not poisoned");
        *map.entry(cmd.to_string()).or_insert(0) += 1;
    }

    /// 幂等快照（BTreeMap 保证排序，复现/对账用）。
    pub fn snapshot(&self) -> BTreeMap<String, u64> {
        self.inner
            .lock()
            .expect("IgnoreTally lock not poisoned")
            .clone()
    }
}
