//! WS 事件流 → 场次窗 Episode 化（第二段 2B「WS 挂接 + 复盘卡换源」）。
//!
//! 体积备书：超 500 线 = recorder 状态体 + LineRender/Episode 双形态 +
//! 本模块自带的大型钉组（fixture 构建成本随轴走）。缝 = 钉组后移 tests/ 单独包；
//! 真实需求到（第二挂接面/第二事实源）时再动。
//!
//! 承接 `session.rs`（2A）产出的 `WsEvent` 流，把单条 WS 会话切成稳定命名的
//! 弹幕/SC/进场 Episode，规则按执行单批 2 的「WS 场次窗」小节：
//!
//! - **场次窗**：`WsRecorder::attach` 仅在房间在播（live_status==1）时开窗，
//!   起点 = 附着时刻，诚实标记 `window_start:"attach"`；窗口期内收到 `LIVE`
//!   用其 `live_time` 校正起点（标记 `window_start:"live"`——中途附着也能拿到
//!   全场起点）；`PREPARING`（任意 round，含轮播）关窗，end = 收到时刻。
//! - **终局收窗**在 `finish`：`ReconnectExhausted` → end = 末条事件 ts + 未知行
//!   「采录中断：WS 断连未恢复」（绝不为缺段补数）；`TimedOut`（12h 保险丝）→
//!   未知行「保险丝收窗」；AuthFailed/Transport → 收窗不造未知行。
//! - **幂等键**：WS 无平台事件 id，复合键 `(room_id, ts_sec, uid, text_hash16)`，
//!   stable=`ws:{room_id}:{ts_sec}:{uid}:{text_hash16}`；content_version 复用
//!   room_corpus 的 `finalize_episode` 指纹——两条 Episode 生产线同公式撞库语义。
//!   `observed_at` 不参与身份（重跑窗线同 id，仅刷 last_seen）。
//! - **facts 键集纪律**（复盘折叠层契约，keys MUST）：
//!   `{room_id, session:{start_timestamp,end_timestamp,rid:"ws:<win_start>"},
//!   ts, sender_uid_mid, uname, ts_source, window_start}`；SC 增 `price`、
//!   entry 增 `interact_kind`。刻意**不产出** `creator_name` / `tags` /
//!   `platform_category`（三键触发现货实体边，graph/build.rs）——WS 线只挂
//!   房间身份（`_room`）与发送者真实 mid。
//! - **ts_source**：弹幕用 `info[9].ts`（`protocol`）、缺载回落本地受时
//!   （`local_recv`）——幂等键随平台事实走，断线重发/同窗重跑才能撞库去重；
//!   SC 用 `data.start_time`；进场用协议 `timestamp`（同标协议轨）。
//! - **计数**（`counts`，BTreeMap 保序）：`Popularity` 只记账
//!   `popularity_latest` 不产 Episode；`SUPER_CHAT_MESSAGE_DELETE` 只计数
//!   `super_chat_delete`；弹幕/SC/进场各计本源事件数。

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use sha2::{Digest, Sha256};

use crate::episodes::room_corpus::{ROOM_VIEWER_ID, finalize_episode};
use crate::episodes::{Episode, EpisodeField, now_iso, unix_secs_to_iso};

use super::message::WsEvent;
use super::session::SessionEnd;

/// WS 弹幕 Episode 的 source / event_type（复盘折叠层按此换源）。
pub const SOURCE_WS_DANMAKU: &str = "live_ws_danmaku";
pub const EVENT_WS_DANMAKU: &str = "live_ws_danmaku";
/// WS 醒目留言 Episode 的 source / event_type。
pub const SOURCE_WS_SC: &str = "live_ws_sc";
pub const EVENT_WS_SC: &str = "live_ws_sc";
/// WS 进场 Episode 的 source / event_type。
pub const SOURCE_WS_ENTRY: &str = "live_ws_entry";
pub const EVENT_WS_ENTRY: &str = "live_ws_entry";
/// WS 礼物 Episode 的 source / event_type（复盘折叠同进场轨：进窗轨不进发言轨）。
pub const SOURCE_WS_GIFT: &str = "live_ws_gift";
pub const EVENT_WS_GIFT: &str = "live_ws_gift";
/// WS 上舰 Episode 的 source / event_type。
pub const SOURCE_WS_GUARD_BUY: &str = "live_ws_guardbuy";
pub const EVENT_WS_GUARD_BUY: &str = "live_ws_guardbuy";
/// WS 上舰播报 Episode 的 source / event_type。
pub const SOURCE_WS_TOAST: &str = "live_ws_toast";
pub const EVENT_WS_TOAST: &str = "live_ws_toast";

/// 场次窗会话 rid 前缀：`ws:{win_start}`（复盘折叠层：WS 源优先于回放束）。
pub const SESSION_RID_PREFIX: &str = "ws";

/// 场次窗起点诚实标记（随窗入 facts）。
pub const WINDOW_START_ATTACH: &str = "attach";
pub const WINDOW_START_LIVE: &str = "live";

/// ts_source 取值：协议自带时间戳 / 本地受时（落 facts 的诚实来源标注）。
pub const TS_SOURCE_PROTOCOL: &str = "protocol";
pub const TS_SOURCE_LOCAL_RECV: &str = "local_recv";

/// 未知行文案（随 `WsWindowCapture.unknowns` 上送，诚实面不补数）。
pub const UNKNOWN_INTERRUPTED: &str = "采录中断：WS 断连未恢复";
pub const UNKNOWN_FAILSAFE: &str = "保险丝收窗";

/// counts 记账键。
pub const COUNT_POPULARITY_LATEST: &str = "popularity_latest";
pub const COUNT_SC_DELETE: &str = "super_chat_delete";
pub const COUNT_DANMAKU: &str = "danmaku";
pub const COUNT_SC: &str = "super_chat";
pub const COUNT_ENTRY: &str = "interact";
pub const COUNT_GIFT: &str = "gift";
pub const COUNT_GUARD_BUY: &str = "guard_buy";
pub const COUNT_TOAST: &str = "toast";

/// text 首 16 位 hex（sha256）——幂等键末段（WS 无平台事件 id 的替代指纹；
/// 空文本（进场/空弹幕）亦按空串散列，窗口内同人同秒同文本同指纹）。
pub fn text_hash16(text: &str) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        write!(out, "{byte:02x}").expect("write to String never fails");
    }
    out
}

/// 场次窗终局原因（`WsWindowCapture.end_reason`，session 随行）。
pub const END_REASON_PREPARING: &str = "preparing";
pub const END_REASON_RECONNECT_EXHAUSTED: &str = "reconnect_exhausted";
pub const END_REASON_TIMED_OUT: &str = "timed_out";
pub const END_REASON_AUTH_FAILED: &str = "auth_failed";
pub const END_REASON_TRANSPORT: &str = "transport";

/// 线族判定（投影分派：WS 事件六产线各走各的 source/facts 键集）。
#[derive(Clone, Copy)]
enum LineKind {
    Danmaku,
    Sc,
    Entry,
    Gift,
    GuardBuy,
    Toast,
}

/// 单条 WS 场线（投影中间形态：源判定 + 事实键由 `WsRecorder::project` 完成）。
struct LineRender<'a> {
    kind: LineKind,
    uid: &'a str,
    uname: &'a str,
    text: &'a str,
    ts_sec: i64,
    ts_source: &'a str,
    /// 付费金额（人民币元）——仅 SC/上舰轨消费；礼物的币轨单价走 extra，不混键。
    price: Option<f64>,
    interact_kind: Option<u32>,
    /// 族专属事实键（礼物面/播报面）——原样落 facts，键名纪律同幕头注。
    extra: Vec<(&'static str, Value)>,
}

/// WS 场窗记录器：`attach` 开窗（在播校验）→ `on_event` 逐事投影 →
/// `finish` 收窗成片 + 账目（未知行/计数/终点）。
///
/// 生命周期串起 `WsRecorder::attach(room_id, now_ts, live_status_now)` →
/// `on_event` 流 →（`Preparing` 或终局兜底）→ `finish(end, now_ts)` 产出
/// `WsWindowCapture`。服务端一个 run 一个记录器。
#[derive(Debug)]
pub struct WsRecorder {
    room_id: i64,
    window_start_ts: i64,
    window_end_ts: Option<i64>,
    window_origin: &'static str,
    lines: Vec<Episode>,
    unknowns: Vec<String>,
    counts: BTreeMap<String, i64>,
}

impl WsRecorder {
    /// 开窗：仅当 `live_status_now == 1`（房间在播）产出记录器。
    /// `now_ts` 为附着时刻（unix 秒），同时是诚实起点 `window_start:"attach"`
    /// （收到 `LIVE` 时窗内校正为 `"live"`）。
    pub fn attach(room_id: i64, now_ts: i64, live_status_now: i64) -> Option<Self> {
        if live_status_now != 1 {
            return None;
        }
        let now_ts = now_ts.max(0);
        Some(WsRecorder {
            room_id,
            window_start_ts: now_ts,
            window_end_ts: None,
            window_origin: WINDOW_START_ATTACH,
            lines: Vec::new(),
            unknowns: Vec::new(),
            counts: BTreeMap::new(),
        })
    }

    /// 当前已知的场次起点与起点诚实标记（run 层记账/对账用）。
    pub fn window_start(&self) -> (i64, &'static str) {
        (self.window_start_ts, self.window_origin)
    }

    /// 投喂一个 WS 事件。`recv_ts` 为本地收到时刻（unix 秒）。
    ///
    /// - `LIVE`：校正场次起点为 `live_time`（校正一次，此后重复 LIVE 不再移动）。
    /// - `PREPARING`：关窗，end=收到时刻（此后事件一律忽略，窗即锁）。
    /// - `Popularity`：只记 `popularity_latest`；`SuperChatDelete`：只计数。
    pub fn on_event(&mut self, ev: &WsEvent, recv_ts: i64) {
        if self.window_end_ts.is_some() {
            // 窗关即锁：Preparing 之后的零散弹幕归下一个 run，本窗不吃。
            return;
        }
        match ev {
            WsEvent::Live { live_time } => {
                if *live_time > 0 && self.window_origin != WINDOW_START_LIVE {
                    self.window_start_ts = *live_time;
                    self.window_origin = WINDOW_START_LIVE;
                }
            }
            WsEvent::Danmaku {
                uid,
                uname,
                text,
                ts,
            } => {
                *self.counts.entry(COUNT_DANMAKU.to_string()).or_insert(0) += 1;
                // 平台时戳（info[9].ts）优先：幂等键随平台事实走——断线重发/同窗
                // 重跑撞库去重才成立；缺载才落本地收到时刻并自标 ts_source。
                let (ts_sec, ts_source) = match ts.filter(|t| *t > 0) {
                    Some(t) => (t, TS_SOURCE_PROTOCOL),
                    None => (recv_ts, TS_SOURCE_LOCAL_RECV),
                };
                if let Some(ep) = self.project(&LineRender {
                    kind: LineKind::Danmaku,
                    uid: uid.as_str(),
                    uname: uname.as_str(),
                    text: text.as_str(),
                    ts_sec,
                    ts_source,
                    price: None,
                    interact_kind: None,
                    extra: Vec::new(),
                }) {
                    self.lines.push(ep);
                }
            }
            WsEvent::SuperChat {
                uid,
                uname,
                text,
                price,
                start_time,
            } => {
                *self.counts.entry(COUNT_SC.to_string()).or_insert(0) += 1;
                let (ts_sec, ts_source) = match start_time.filter(|t| *t > 0) {
                    Some(t) => (t, TS_SOURCE_PROTOCOL),
                    None => (recv_ts, TS_SOURCE_LOCAL_RECV),
                };
                if let Some(ep) = self.project(&LineRender {
                    kind: LineKind::Sc,
                    uid: uid.as_str(),
                    uname: uname.as_str(),
                    text: text.as_str(),
                    ts_sec,
                    ts_source,
                    price: Some(*price),
                    interact_kind: None,
                    extra: Vec::new(),
                }) {
                    self.lines.push(ep);
                }
            }
            WsEvent::Interact {
                kind,
                uid,
                uname,
                ts,
            } => {
                *self.counts.entry(COUNT_ENTRY.to_string()).or_insert(0) += 1;
                let (ts_sec, ts_source) = if *ts > 0 {
                    (*ts, TS_SOURCE_PROTOCOL)
                } else {
                    (recv_ts, TS_SOURCE_LOCAL_RECV)
                };
                if let Some(ep) = self.project(&LineRender {
                    kind: LineKind::Entry,
                    uid: uid.as_str(),
                    uname: uname.as_str(),
                    text: "",
                    ts_sec,
                    ts_source,
                    price: None,
                    interact_kind: Some(*kind),
                    extra: Vec::new(),
                }) {
                    self.lines.push(ep);
                }
            }
            WsEvent::Gift {
                uid,
                uname,
                gift_name,
                num,
                price,
                total_coin,
                coin_type,
                ts,
            } => {
                *self.counts.entry(COUNT_GIFT.to_string()).or_insert(0) += 1;
                let (ts_sec, ts_source) = match ts.filter(|t| *t > 0) {
                    Some(t) => (t, TS_SOURCE_PROTOCOL),
                    None => (recv_ts, TS_SOURCE_LOCAL_RECV),
                };
                // 币轨原价原样落 facts（price=单价、total_coin=金瓜子总额、coin_type=
                // gold/silver）——换算成元的解读权归消费侧（复盘/报表），事实层不动算。
                if let Some(ep) = self.project(&LineRender {
                    kind: LineKind::Gift,
                    uid: uid.as_str(),
                    uname: uname.as_str(),
                    text: "",
                    ts_sec,
                    ts_source,
                    price: None,
                    interact_kind: None,
                    extra: vec![
                        ("gift_name", Value::String(gift_name.clone())),
                        ("gift_num", serde_json::json!(*num)),
                        ("gift_price", serde_json::json!(*price)),
                        ("total_coin", serde_json::json!(*total_coin)),
                        ("coin_type", Value::String(coin_type.clone())),
                    ],
                }) {
                    self.lines.push(ep);
                }
            }
            WsEvent::GuardBuy {
                uid,
                uname,
                guard_level,
                gift_name,
                num,
                price,
                ts,
            } => {
                *self.counts.entry(COUNT_GUARD_BUY.to_string()).or_insert(0) += 1;
                let (ts_sec, ts_source) = match ts.filter(|t| *t > 0) {
                    Some(t) => (t, TS_SOURCE_PROTOCOL),
                    None => (recv_ts, TS_SOURCE_LOCAL_RECV),
                };
                if let Some(ep) = self.project(&LineRender {
                    kind: LineKind::GuardBuy,
                    uid: uid.as_str(),
                    uname: uname.as_str(),
                    text: "",
                    ts_sec,
                    ts_source,
                    price: Some(*price),
                    interact_kind: None,
                    extra: vec![
                        ("guard_level", serde_json::json!(*guard_level)),
                        ("gift_name", Value::String(gift_name.clone())),
                        ("gift_num", serde_json::json!(*num)),
                    ],
                }) {
                    self.lines.push(ep);
                }
            }
            WsEvent::Toast {
                uid,
                uname,
                role_name,
                guard_level,
                ts,
            } => {
                *self.counts.entry(COUNT_TOAST.to_string()).or_insert(0) += 1;
                let (ts_sec, ts_source) = match ts.filter(|t| *t > 0) {
                    Some(t) => (t, TS_SOURCE_PROTOCOL),
                    None => (recv_ts, TS_SOURCE_LOCAL_RECV),
                };
                if let Some(ep) = self.project(&LineRender {
                    kind: LineKind::Toast,
                    uid: uid.as_str(),
                    uname: uname.as_str(),
                    text: "",
                    ts_sec,
                    ts_source,
                    price: None,
                    interact_kind: None,
                    extra: vec![
                        ("role_name", Value::String(role_name.clone())),
                        ("guard_level", serde_json::json!(*guard_level)),
                    ],
                }) {
                    self.lines.push(ep);
                }
            }
            WsEvent::Preparing { .. } => {
                if self.window_end_ts.is_none() {
                    self.window_end_ts = Some(recv_ts.max(self.window_start_ts));
                }
            }
            WsEvent::Popularity { value } => {
                self.counts
                    .insert(COUNT_POPULARITY_LATEST.to_string(), i64::from(*value));
            }
            WsEvent::SuperChatDelete { .. } => {
                *self.counts.entry(COUNT_SC_DELETE.to_string()).or_insert(0) += 1;
            }
            WsEvent::AuthAck { .. } => {}
        }
    }

    /// 收窗。`end` 为会话终局原因，`now_ts` 为收尾时刻（unix 秒）。
    ///
    /// - `PREPARING` 已到（`window_end_ts` 已盖）→ 用之；`Closed` 兜底 = `now_ts`。
    /// - `ReconnectExhausted` → end = 末条事件 ts + 未知「采录中断」。
    /// - `TimedOut` → end = `now_ts` + 未知「保险丝收窗」。
    /// - AuthFailed/Transport → end = `now_ts`，不造未知行。
    ///
    /// 收窗同时以**最终场次窗事实**（校正后起点、终点、`ws:{start}` rid、起点
    /// 诚实标记）统一重指纹每线——`LIVE` 校正可能晚于首行入窗，窗内任何阶段
    /// 投影的状态都不是定稿，撞库只看 `finish` 的产出。
    pub fn finish(mut self, end: &SessionEnd, now_ts: i64) -> WsWindowCapture {
        let end_reason = end_reason_of(end);
        let (end_ts, close_unknowns) = match self.window_end_ts {
            Some(ts) => (ts, Vec::new()),
            None => match end {
                SessionEnd::Closed => (now_ts, Vec::new()),
                SessionEnd::ReconnectExhausted => {
                    let last_ts = self
                        .lines
                        .last()
                        .and_then(|ep| ep.platform_facts.get("ts").and_then(Value::as_i64))
                        .filter(|ts| *ts > 0)
                        .unwrap_or(now_ts);
                    (last_ts, vec![UNKNOWN_INTERRUPTED.to_string()])
                }
                SessionEnd::TimedOut => (now_ts, vec![UNKNOWN_FAILSAFE.to_string()]),
                SessionEnd::AuthFailed { .. } | SessionEnd::Transport(_) => (now_ts, Vec::new()),
            },
        };
        let end_ts = end_ts.max(self.window_start_ts);
        self.window_end_ts = Some(end_ts);
        self.unknowns.extend(close_unknowns);

        let episodes = self
            .lines
            .iter()
            .map(|line| refinalize(line, self.window_start_ts, end_ts, self.window_origin))
            .collect();

        let session = serde_json::json!({
            "room_id": self.room_id.to_string(),
            "rid": format!("{SESSION_RID_PREFIX}:{}", self.window_start_ts),
            "start_timestamp": self.window_start_ts,
            "end_timestamp": end_ts,
            "window_start": self.window_origin,
            "end_reason": end_reason,
        });
        WsWindowCapture {
            episodes,
            session,
            unknowns: self.unknowns,
            counts: self.counts,
            end_reason: end_reason.to_string(),
        }
    }

    /// 单线 → Episode 草稿。uid 空 / ts ≤0 的脏线跳过（无身份即无幂等锚）。
    /// 场次窗 facts 以「当前已知值」落稿（终点未知先落线时刻），`finish`
    /// 以最终窗事实统一重指纹（`refinalize`），此处 id 只作中间态。
    fn project(&self, line: &LineRender<'_>) -> Option<Episode> {
        if line.uid.is_empty() || line.uid == "0" || line.ts_sec <= 0 {
            // uid 空或平台 0（匿名/播报占位）都无身份锚——不产 Episode。
            return None;
        }
        let (source, event_type) = match line.kind {
            LineKind::Danmaku => (SOURCE_WS_DANMAKU, EVENT_WS_DANMAKU),
            LineKind::Sc => (SOURCE_WS_SC, EVENT_WS_SC),
            LineKind::Entry => (SOURCE_WS_ENTRY, EVENT_WS_ENTRY),
            LineKind::Gift => (SOURCE_WS_GIFT, EVENT_WS_GIFT),
            LineKind::GuardBuy => (SOURCE_WS_GUARD_BUY, EVENT_WS_GUARD_BUY),
            LineKind::Toast => (SOURCE_WS_TOAST, EVENT_WS_TOAST),
        };
        let fields = if line.text.is_empty() {
            Vec::new()
        } else {
            vec![EpisodeField {
                path: "text".to_string(),
                text: line.text.to_string(),
                kind: "text".to_string(),
            }]
        };
        let end_ts = self.window_end_ts.unwrap_or(line.ts_sec);
        let mut facts = Map::new();
        facts.insert(
            "room_id".to_string(),
            Value::String(self.room_id.to_string()),
        );
        facts.insert(
            "session".to_string(),
            serde_json::json!({
                "start_timestamp": self.window_start_ts,
                "end_timestamp": end_ts,
                "rid": format!("{SESSION_RID_PREFIX}:{}", self.window_start_ts),
            }),
        );
        facts.insert("ts".to_string(), serde_json::json!(line.ts_sec));
        facts.insert(
            "sender_uid_mid".to_string(),
            Value::String(line.uid.to_string()),
        );
        facts.insert("uname".to_string(), Value::String(line.uname.to_string()));
        facts.insert(
            "ts_source".to_string(),
            Value::String(line.ts_source.to_string()),
        );
        facts.insert(
            "window_start".to_string(),
            Value::String(self.window_origin.to_string()),
        );
        if let Some(price) = line.price {
            facts.insert("price".to_string(), serde_json::json!(price));
        }
        if let Some(kind) = line.interact_kind {
            facts.insert("interact_kind".to_string(), serde_json::json!(kind));
        }
        for (key, value) in &line.extra {
            facts.insert((*key).to_string(), value.clone());
        }
        Some(finalize_episode(
            ROOM_VIEWER_ID,
            &format!(
                "{SESSION_RID_PREFIX}:{}:{}:{}:{}",
                self.room_id,
                line.ts_sec,
                line.uid,
                text_hash16(line.text)
            ),
            source,
            event_type,
            &unix_secs_to_iso(Some(self.window_start_ts)),
            fields,
            Value::Object(facts),
            &now_iso(),
        ))
    }
}

/// 终局 → `end_reason` 常量。
fn end_reason_of(end: &SessionEnd) -> &'static str {
    match end {
        SessionEnd::Closed => END_REASON_PREPARING,
        SessionEnd::ReconnectExhausted => END_REASON_RECONNECT_EXHAUSTED,
        SessionEnd::TimedOut => END_REASON_TIMED_OUT,
        SessionEnd::AuthFailed { .. } => END_REASON_AUTH_FAILED,
        SessionEnd::Transport(_) => END_REASON_TRANSPORT,
    }
}

/// 单线重指纹：以最终场次窗事实（起点/终点/rid/起点诚实标记）重投影。
/// 幂等稳定键从 facts 复算（room_id/ts/sender_uid_mid/text_hash16），
/// 与 `project` 的草稿 id 不同（终点/起点校正晚定），撞库只认本产出。
fn refinalize(line: &Episode, start_ts: i64, end_ts: i64, origin: &str) -> Episode {
    let facts = line
        .platform_facts
        .as_object()
        .expect("WS line platform_facts is an object");
    let room_id = facts
        .get("room_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let ts_sec = facts.get("ts").and_then(Value::as_i64).unwrap_or(0);
    let uid = facts
        .get("sender_uid_mid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let text = line.field_text("text").unwrap_or("").to_string();
    let mut new_facts = facts.clone();
    new_facts.insert(
        "session".to_string(),
        serde_json::json!({
            "start_timestamp": start_ts,
            "end_timestamp": end_ts,
            "rid": format!("{SESSION_RID_PREFIX}:{start_ts}"),
        }),
    );
    new_facts.insert(
        "window_start".to_string(),
        Value::String(origin.to_string()),
    );
    finalize_episode(
        ROOM_VIEWER_ID,
        &format!(
            "{SESSION_RID_PREFIX}:{room_id}:{ts_sec}:{uid}:{}",
            text_hash16(&text)
        ),
        &line.source,
        &line.event_type,
        &unix_secs_to_iso(Some(start_ts)),
        line.fields.clone(),
        Value::Object(new_facts),
        &now_iso(),
    )
}

/// WS 场窗捕获物：一次收窗的全部产出（供 run 挂接入账/上送 outcome）。
#[derive(Debug, Clone)]
pub struct WsWindowCapture {
    /// 最终定稿的窗内线（撞库入账对象）。
    pub episodes: Vec<Episode>,
    /// 窗汇总（rid/起点/终点/起点诚实标记/终局原因/房间号）。
    pub session: Value,
    /// 未知行（诚实面：采录中断/保险丝等，随 outcome 上送）。
    pub unknowns: Vec<String>,
    /// 事件计数（popularity_latest 等）。
    pub counts: BTreeMap<String, i64>,
    /// 终局原因（`END_REASON_*`）。
    pub end_reason: String,
}

/// WS 场窗 Episode 入账：房间语料通道（`ingest_room_corpus`，同一 upsert
/// 纪律：`viewer:_room` 守卫节点 + `ingest_platform_facts` 逐条撞库）。
///
/// 幂等键纪律（同窗重跑/断线重发背靠背相邻窗）：同一平台行（同 room/ts/uid/文本）
/// 在不同窗会因 session rid 漂移产生不同 content_version——撞库身份因此归 stable
/// 前缀（`touch_episode_by_identity`）：同身份已在库 → 只刷 last_seen，本窗该行不进
/// 语料通道（事实层行数守恒，复盘四个数不虚增）。
pub fn ingest_ws_window(
    store: &crate::graph::Store,
    run_id: &str,
    capture: &WsWindowCapture,
) -> crate::graph::store::Result<()> {
    let mut fresh: Vec<Episode> = Vec::with_capacity(capture.episodes.len());
    for episode in &capture.episodes {
        // id = episode:{viewer}:{stable}:{content_version} —— 剥尾段即 stable 前缀。
        let identity = episode
            .episode_id
            .rsplit_once(':')
            .map(|(prefix, _)| prefix)
            .unwrap_or(&episode.episode_id);
        // immutable 守卫降格不得：同身份比对正文摘文（与 upsert 同一 canon 口径）。
        let fields_canon = crate::episodes::json_canon(&Value::Array(
            episode
                .fields
                .iter()
                .map(crate::episodes::EpisodeField::to_json)
                .collect::<Vec<_>>(),
        ));
        if store.touch_episode_by_identity(identity, &fields_canon)? {
            continue;
        }
        fresh.push(episode.clone());
    }
    crate::episodes::room_corpus::ingest_room_corpus(store, run_id, &fresh)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    /// 场次窗固定流：attach(在播) → LIVE 校正 → 一弹幕 → PREPARING 关窗 → finish。
    #[test]
    fn live_corrected_window_materializes_one_danmaku() {
        let mut rec = WsRecorder::attach(9222, 1_000_000, 1).expect("在播开窗");
        rec.on_event(&WsEvent::Live { live_time: 999_000 }, 1_000_001);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u-1".into(),
                uname: "某人".into(),
                text: "好耶".into(),
                ts: None,
            },
            1_000_002,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_003);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_003);

        assert_eq!(cap.end_reason, END_REASON_PREPARING);
        assert_eq!(cap.episodes.len(), 1);
        let ep = &cap.episodes[0];
        assert_eq!(ep.source, SOURCE_WS_DANMAKU);
        assert_eq!(ep.viewer_id, ROOM_VIEWER_ID);
        assert_eq!(
            ep.platform_facts["session"]["start_timestamp"],
            json!(999_000)
        );
        assert_eq!(
            ep.platform_facts["session"]["end_timestamp"],
            json!(1_000_003)
        );
        assert_eq!(ep.platform_facts["session"]["rid"], json!("ws:999000"));
        assert_eq!(ep.platform_facts["window_start"], json!(WINDOW_START_LIVE));
        assert_eq!(ep.published_at, unix_secs_to_iso(Some(999_000)));
        assert!(
            ep.episode_id
                .starts_with("episode:_room:ws:9222:1000002:u-1:")
        );
        // 窗口汇总契约
        assert_eq!(cap.session["rid"], json!("ws:999000"));
        assert_eq!(cap.session["start_timestamp"], json!(999_000));
        assert_eq!(cap.session["end_timestamp"], json!(1_000_003));
        assert_eq!(cap.session["window_start"], json!(WINDOW_START_LIVE));
    }

    #[test]
    fn attach_only_opens_when_room_is_live() {
        assert!(
            WsRecorder::attach(7, 1_000_000, 0).is_none(),
            "live_status 0 不开窗"
        );
        assert!(
            WsRecorder::attach(7, 1_000_000, 2).is_none(),
            "live_status 2 不开窗"
        );
        let rec = WsRecorder::attach(7, 1_000_000, 1).expect("live_status 1 开窗");
        assert_eq!(rec.window_start(), (1_000_000, WINDOW_START_ATTACH));
    }

    #[test]
    fn attach_window_without_live_uses_attach_start() {
        let mut rec = WsRecorder::attach(9222, 1_000_000, 1).unwrap();
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u-1".into(),
                uname: String::new(),
                text: "附着即播".into(),
                ts: None,
            },
            1_000_001,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_002);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_002);
        assert_eq!(cap.episodes.len(), 1);
        let ep = &cap.episodes[0];
        assert_eq!(
            ep.platform_facts["session"]["start_timestamp"],
            json!(1_000_000),
            "窗起点=附着时刻（诚实标记 attach）"
        );
        assert_eq!(
            ep.platform_facts["window_start"],
            json!(WINDOW_START_ATTACH)
        );
        assert_eq!(ep.published_at, unix_secs_to_iso(Some(1_000_000)));
    }

    #[test]
    fn facts_key_set_is_spec_conformant() {
        let mut rec = WsRecorder::attach(7, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::Live { live_time: 999_000 }, 1_000_001);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u1".into(),
                uname: "名".into(),
                text: "好".into(),
                ts: None,
            },
            1_000_002,
        );
        rec.on_event(
            &WsEvent::SuperChat {
                uid: "u2".into(),
                uname: "S".into(),
                text: "大气".into(),
                price: 30.0,
                start_time: Some(1_000_003),
            },
            1_000_003,
        );
        rec.on_event(
            &WsEvent::Interact {
                kind: 1,
                uid: "u3".into(),
                uname: "进".into(),
                ts: 1_000_010,
            },
            1_000_004,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_011);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_011);
        assert_eq!(cap.episodes.len(), 3);
        assert_eq!(cap.counts.get(COUNT_DANMAKU), Some(&1));
        assert_eq!(cap.counts.get(COUNT_SC), Some(&1));
        assert_eq!(cap.counts.get(COUNT_ENTRY), Some(&1));

        let dan = &cap.episodes[0];
        let mut keys: Vec<&str> = dan
            .platform_facts
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "room_id",
                "sender_uid_mid",
                "session",
                "ts",
                "ts_source",
                "uname",
                "window_start"
            ],
            "弹幕 facts 键集（SC/entry 各增一专键）"
        );
        assert_eq!(dan.platform_facts["sender_uid_mid"], json!("u1"));
        assert_eq!(dan.platform_facts["uname"], json!("名"));
        assert_eq!(dan.platform_facts["ts_source"], json!(TS_SOURCE_LOCAL_RECV));
        assert_eq!(dan.platform_facts["ts"], json!(1_000_002));
        assert_eq!(
            dan.platform_facts["session"]["end_timestamp"],
            json!(1_000_011)
        );

        let sc = &cap.episodes[1];
        assert_eq!(sc.source, SOURCE_WS_SC);
        assert_eq!(sc.platform_facts["price"], json!(30.0));
        assert_eq!(
            sc.platform_facts["ts"],
            json!(1_000_003),
            "SC 用 data.start_time"
        );
        assert_eq!(sc.platform_facts["ts_source"], json!(TS_SOURCE_PROTOCOL));

        let entry = &cap.episodes[2];
        assert_eq!(entry.source, SOURCE_WS_ENTRY);
        assert_eq!(entry.platform_facts["interact_kind"], json!(1));
        assert_eq!(
            entry.platform_facts["ts"],
            json!(1_000_010),
            "进场用协议 timestamp"
        );
        assert_eq!(entry.platform_facts["ts_source"], json!(TS_SOURCE_PROTOCOL));
        assert!(entry.fields.is_empty(), "进场线零 fields");
    }

    #[test]
    fn sc_without_start_time_falls_back_to_local_recv() {
        let mut rec = WsRecorder::attach(7, 1_000_000, 1).unwrap();
        rec.on_event(
            &WsEvent::SuperChat {
                uid: "u2".into(),
                uname: "S".into(),
                text: "大气".into(),
                price: 30.0,
                start_time: None,
            },
            1_000_002,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_003);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_003);
        let sc = &cap.episodes[0];
        assert_eq!(
            sc.platform_facts["ts"],
            json!(1_000_002),
            "缺段回落本地受时"
        );
        assert_eq!(sc.platform_facts["ts_source"], json!(TS_SOURCE_LOCAL_RECV));
    }

    #[test]
    fn episode_id_is_idempotent_over_same_window() {
        fn drive(close_at: i64) -> WsWindowCapture {
            let mut rec = WsRecorder::attach(9222, 1_000_000, 1).unwrap();
            rec.on_event(&WsEvent::Live { live_time: 995_000 }, 1_000_001);
            rec.on_event(
                &WsEvent::Danmaku {
                    uid: "u-1".into(),
                    uname: String::new(),
                    text: "好".into(),
                    ts: None,
                },
                1_000_001,
            );
            rec.on_event(&WsEvent::Preparing { round: 1 }, close_at);
            rec.finish(&SessionEnd::Closed, close_at)
        }
        let a = drive(1_000_002);
        let b = drive(1_000_002);
        assert_eq!(
            a.episodes[0].episode_id, b.episodes[0].episode_id,
            "同窗同点重跑 → 同 id（幂等）"
        );
        // 收尾时刻变 = 场次窗 content 变（session.end_timestamp 进指纹）——id 整体
        // 变，但幂等键前段（stable：room/ts/uid/text_hash16）必须稳定不动。
        let c = drive(2_000_001);
        assert_ne!(a.episodes[0].episode_id, c.episodes[0].episode_id);
        // 闭包返回引用的生命周期不解耦（&input→&output 擦除失败），用 fn 项走 elision。
        fn stable_prefix(id: &str) -> &str {
            let split = id.rfind(':').expect("episode_id 含尾段哈希");
            &id[..split]
        }
        assert_eq!(
            stable_prefix(&a.episodes[0].episode_id),
            stable_prefix(&c.episodes[0].episode_id)
        );
    }

    #[test]
    fn ws_episodes_never_emit_stock_face_keys() {
        let mut rec = WsRecorder::attach(1, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::Live { live_time: 995_000 }, 1_000_001);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u-5".into(),
                uname: String::new(),
                text: "哈哈".into(),
                ts: None,
            },
            1_000_002,
        );
        rec.on_event(
            &WsEvent::SuperChat {
                uid: "u-6".into(),
                uname: String::new(),
                text: "了".into(),
                price: 100.0,
                start_time: Some(1_000_003),
            },
            1_000_003,
        );
        rec.on_event(
            &WsEvent::Interact {
                kind: 1,
                uid: "u-7".into(),
                uname: String::new(),
                ts: 1_000_004,
            },
            1_000_003,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_005);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_005);
        assert_eq!(cap.episodes.len(), 3);
        for ep in &cap.episodes {
            for banned in ["creator_name", "tags", "platform_category"] {
                assert!(
                    ep.platform_facts.get(banned).is_none(),
                    "{banned} 不得出现在 WS Episode（实体边红线）：{}",
                    ep.platform_facts
                );
            }
            assert_eq!(ep.viewer_id, ROOM_VIEWER_ID, "WS 线挂房间身份");
        }
    }

    #[test]
    fn popularity_only_window_counts_and_no_episodes() {
        let mut rec = WsRecorder::attach(5, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::Popularity { value: 100 }, 1_000_001);
        rec.on_event(&WsEvent::Popularity { value: 345 }, 1_000_002);
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_003);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_003);
        assert!(cap.episodes.is_empty(), "纯人气窗零 Episode");
        assert_eq!(cap.counts.get(COUNT_POPULARITY_LATEST), Some(&345));
    }

    #[test]
    fn super_chat_delete_counts_but_no_episode() {
        let mut rec = WsRecorder::attach(5, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::SuperChatDelete { ids: vec![1, 2] }, 1_000_001);
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_002);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_002);
        assert!(cap.episodes.is_empty());
        assert_eq!(cap.counts.get(COUNT_SC_DELETE), Some(&1));
    }

    #[test]
    fn reconnect_exhausted_closes_at_last_event_ts_with_unknown() {
        let mut rec = WsRecorder::attach(3, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::Live { live_time: 999_000 }, 1_000_001);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u1".into(),
                uname: String::new(),
                text: "前".into(),
                ts: None,
            },
            1_000_010,
        );
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u2".into(),
                uname: String::new(),
                text: "后".into(),
                ts: None,
            },
            1_000_012,
        );
        let cap = rec.finish(&SessionEnd::ReconnectExhausted, 9_999_999);
        assert_eq!(cap.end_reason, END_REASON_RECONNECT_EXHAUSTED);
        assert_eq!(
            cap.session["end_timestamp"],
            json!(1_000_012),
            "收窗时刻=末条事件 ts"
        );
        assert!(
            cap.unknowns.iter().any(|u| u.contains(UNKNOWN_INTERRUPTED)),
            "断连未恢复 → 未知行：{:?}",
            cap.unknowns
        );
        for ep in &cap.episodes {
            assert_eq!(
                ep.platform_facts["session"]["end_timestamp"],
                json!(1_000_012),
                "线 facts 以最终收窗为准"
            );
        }
    }

    #[test]
    fn timed_out_finish_adds_failsafe_unknown() {
        let mut rec = WsRecorder::attach(3, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::Live { live_time: 995_000 }, 1_000_001);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u1".into(),
                uname: String::new(),
                text: "挂".into(),
                ts: None,
            },
            1_000_010,
        );
        let cap = rec.finish(&SessionEnd::TimedOut, 2_000_000);
        assert_eq!(cap.end_reason, END_REASON_TIMED_OUT);
        assert_eq!(cap.session["end_timestamp"], json!(2_000_000));
        assert!(
            cap.unknowns.iter().any(|u| u.contains(UNKNOWN_FAILSAFE)),
            "保险丝收窗 → 未知行：{:?}",
            cap.unknowns
        );
    }

    #[test]
    fn auth_failed_and_preparing_closed_window_has_no_unknown() {
        let mut rec = WsRecorder::attach(5, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::AuthAck { code: -101 }, 1_000_001);
        let cap = rec.finish(&SessionEnd::AuthFailed { code: -101 }, 1_000_002);
        assert_eq!(cap.end_reason, END_REASON_AUTH_FAILED);
        assert!(cap.episodes.is_empty());
        assert!(cap.unknowns.is_empty());
    }

    #[test]
    fn dirty_lines_are_skipped_but_window_still_materializes() {
        let mut rec = WsRecorder::attach(5, 1_000_000, 1).unwrap();
        // uid 空 / ts 0 都不产 Episode（无幂等锚）
        rec.on_event(
            &WsEvent::Danmaku {
                uid: String::new(),
                uname: String::new(),
                text: "坏".into(),
                ts: None,
            },
            0,
        );
        rec.on_event(
            &WsEvent::Interact {
                kind: 1,
                uid: String::new(),
                uname: "x".into(),
                ts: 0,
            },
            1_000_002,
        );
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u9".into(),
                uname: String::new(),
                text: "好".into(),
                ts: None,
            },
            1_000_003,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_004);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_004);
        assert_eq!(
            cap.episodes.len(),
            1,
            "纯脏线不产 Episode: {:?}",
            cap.episodes
        );
    }

    #[test]
    fn ingest_ws_window_dedups_on_rerun() {
        let dir = tempdir().unwrap();
        let store = crate::graph::Store::open(&dir.path().join("graph.sqlite3")).expect("store");
        let mut rec = WsRecorder::attach(9222, 1_000_000, 1).unwrap();
        rec.on_event(&WsEvent::Live { live_time: 999_000 }, 1_000_001);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u-1".into(),
                uname: "甲".into(),
                text: "好耶".into(),
                ts: None,
            },
            1_000_002,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_003);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_003);
        for run in ["run-a", "run-b"] {
            store
                .begin_run_fixed(run, &now_iso(), "test")
                .expect("begin run");
            ingest_ws_window(&store, run, &cap).expect("ingest");
        }
        let rows = crate::graph::query::episodes(&store, ROOM_VIEWER_ID, None)
            .expect("query")
            .len();
        assert_eq!(rows, 1, "重跑同窗追加不重复（撞库只刷 last_seen）");

        // content 变化必须撞库报错（复用 upsert 既有纪律）
        let mut tampered = cap.episodes[0].clone();
        tampered.fields[0].text = "被改写".to_string();
        store
            .begin_run_fixed("run-c", &now_iso(), "test")
            .expect("begin run");
        let err = ingest_ws_window(
            &store,
            "run-c",
            &WsWindowCapture {
                episodes: vec![tampered],
                ..cap.clone()
            },
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("immutable Episode conflict"),
            "撞库拒绝须报错而非静默覆盖: {err}"
        );
    }

    /// 身份收拢真实场景钉（B-C 爆雷面的幂等根）：背靠背两次 collect——两窗附着
    /// 错开一秒，session rid 漂移 → full-id 必然换脸（旧撞库语义下同一平台行必然
    /// 重复入账、复盘四个数虚增）。身份归 stable 前缀后：第二窗同一平台行只刷
    /// last_seen，行数守恒，且行事实保首次所见（首窗版本）。
    #[test]
    fn ingest_ws_window_back_to_back_windows_dedup_same_platform_line() {
        let dir = tempdir().unwrap();
        let store = crate::graph::Store::open(&dir.path().join("graph.sqlite3")).expect("store");
        let danmaku = WsEvent::Danmaku {
            uid: "u-7".into(),
            uname: "丙".into(),
            text: "贴贴".into(),
            ts: Some(1_700_000_000),
        };
        let build_capture = |attach_ts: i64| {
            let mut rec = WsRecorder::attach(77, attach_ts, 1).unwrap();
            rec.on_event(&danmaku, attach_ts + 1);
            rec.on_event(&WsEvent::Preparing { round: 1 }, attach_ts + 2);
            rec.finish(&SessionEnd::Closed, attach_ts + 2)
        };
        let first = build_capture(1_000_000);
        let second = build_capture(1_000_001);
        assert_ne!(
            first.episodes[0].episode_id, second.episodes[0].episode_id,
            "钉前提：ID 尾段=content_version，窗事实漂移必换脸"
        );
        store
            .begin_run_fixed("run-a", &now_iso(), "test")
            .expect("begin run");
        ingest_ws_window(&store, "run-a", &first).expect("首窗入账");
        store
            .begin_run_fixed("run-b", &now_iso(), "test")
            .expect("begin run");
        ingest_ws_window(&store, "run-b", &second).expect("邻窗入账");
        let rows = crate::graph::query::episodes(&store, ROOM_VIEWER_ID, None).expect("query");
        assert_eq!(rows.len(), 1, "相邻窗同一平台行不得重复入账：{rows:?}");
        assert_eq!(
            rows[0]["episode_id"],
            json!(first.episodes[0].episode_id),
            "行事实保首次所见（新窗不覆盖）：{rows:?}"
        );
    }
}
