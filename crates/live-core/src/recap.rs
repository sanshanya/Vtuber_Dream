//! 下播复盘卡·纯规则层（迭代细则 v1 §1）：从 `_room` 房间语料 Episode 的
//! platform_facts 直接聚四个数——**全部程序事实，零 AI**。
//!
//! 体积备书：超 500 线 = compute_recap 单遍四规则（共享 sessions 归类单元
//! 走遍一次弹幕流）+ fixture 对齐测试 ~1/3。逐规则拆会复制 sessions/normalize 预处理；
//! 拆缝 = 真实需求到（如新增第五数共享窗口）时按规则函数出 `rules.rs`。
//!
//! 四个数（细则原文）：
//! 1. 本场 distinct 发言 uid 数（danmaku+comment 并集）；
//! 2. 回来过的：本场 uid ∩ 前 N 场发言 uid（`RECAP_LOOKBACK_SESSIONS`；
//!    分子分母明示）；
//! 3. 密度峰：`RECAP_PEAK_WINDOW_MINUTES` 分钟窗滑弹幕行数 top-1（带时间戳）；
//! 4. 被复读的句子：归一化文本本场出现 ≥ `RECAP_REPEAT_MIN_COUNT` 次 top-1。
//!
//! 未知行纪律（验收钉④）：「未知的部分」列表**恒存在**——数据面哪里薄就明说
//! （缺时间戳、没有历史场、复读句无达标项是负发现而不是未知，不进未知行）。
//! AI 命名件（peak_name 等）由 pipeline 的终局 Tool Call 另行落进 naming 键；
//! 本模块只造规则体，命名缺位 = null + 未知行（绝不伪造语义——AGENTS §11）。
//!
//! 复盘解耦：出卡不再锁全量感知——`refresh_recap_card` 是 collect 尾与
//! pipeline 尾的共用刷新闻（语料入账→四个数→旧命名留存/作废→落盘，纯规则零 AI）；
//! AI 命名是认知层附加窗：同场次同数面旧命名直接留存（不白跑 AI），数面一动即显式
//! 作废进未知行（绝不拿旧命名盖新数），等下一轮感知重命名。
//!
//! 换轨：场次发现并行扫回放束（live_danmaku）与 WS 窗
//! （live_ws_danmaku / live_ws_sc / live_ws_entry 三源沉降为 sessions + lines）：
//! 时间区间**重叠即折叠**——客观时间事实，绝不做语义合并；折叠界 = [min start,
//! max end]，同夜双写不翻成两场。折叠场次 rid 遵循 WS 优先（`ws:{start}`，主轨），
//! 否则取最早 start 的回放束 rid。
//!
//! 身份轨纪律（冻结共识的复盘侧落实）：身份键 = `轨|uid`（轨 ∈ replay/comment/ws；
//! replay 读 sender_uid_crc，comment 读 mid，ws 读 sender_uid_mid），**跨轨不合并**——
//! 同人跨轨发言按两轨分计，「回来过」也不跨轨认亲（绝不把回放 crc 与真实 mid 混作
//! 同一人）；两轨/三轨并出场必进未知行明示。
//!
//! 窗口诚实面：`window_start:"attach"`（未收 LIVE 的机时附着）经 facts 进未知行。
//! 采录中断/保险丝等终局诚实面**不落 graph facts**（live_ws/episodes.rs contract：
//! 窗原因只随 `WsWindowCapture` 走）——由 run outcome 的 ws_window.unknowns 承载，
//! 本模块不臆造。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::episodes::now_iso;
use crate::graph::Store;
use crate::graph::store::{Result, StoreError};
use crate::live_ws::episodes::{
    SESSION_RID_PREFIX, SOURCE_WS_DANMAKU, SOURCE_WS_ENTRY, SOURCE_WS_GIFT, SOURCE_WS_GUARD_BUY,
    SOURCE_WS_SC, SOURCE_WS_TOAST, WINDOW_START_ATTACH,
};

/// 密度峰窗口（分钟）。依据：细则 §1「10 分钟窗滑弹幕行数 top-1」——
/// 10 分钟是主播节奏的可操作颗粒（一首歌/一段杂谈的长度）。改它须改细则。
pub const RECAP_PEAK_WINDOW_MINUTES: i64 = 10;
/// 复读句达标线（归一化文本本场出现次数 ≥ 此值才计入 top-1 候选）。
/// 依据：细则原文「≥ 3 次」——2 是偶发，3 是复读行为。
pub const RECAP_REPEAT_MIN_COUNT: i64 = 3;
/// 「前 N 场」回看窗。依据：周播节奏 1–2 周内的场次都可能是同批观众；
/// 更老的场次对「回来过」的判定稀释成噪声。细则未钉死数值——命名留档。
pub const RECAP_LOOKBACK_SESSIONS: usize = 10;

/// 身份轨常量（换轨：复盘侧身份键前缀应 trio——replay/comment/ws）。
const TRACK_REPLAY: &str = "replay";
const TRACK_WS: &str = "ws";
const TRACK_COMMENT: &str = "comment";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapSession {
    pub start: String,
    pub end: String,
    pub rid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapPeak {
    pub start: String,
    pub count: i64,
    pub window_minutes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapRepeat {
    pub text: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapReturning {
    /// 本场发言者中在前 N 场出现过的人数（分子）。
    pub count: i64,
    /// 本场发言总人数（分母，明示）。
    pub base: i64,
    /// 实际参考的历史场数。
    pub sessions_back: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapNaming {
    pub peak_name: String,
    pub sentence_name: String,
    pub reuse_line: String,
    pub cut_advice: String,
    pub named_at: String,
}

/// 金额面（当前场次窗）：付费礼物/SC/上舰/播报的规则合计。
///
/// 换算纪律：事实层（Episode facts）只存平台原币轨（total_coin 金瓜子、price
/// 平台原值）；「1000 金瓜子 = 1 元」「coin_type=="gold" 才算付费」的解读权
/// ——同 ws-replay 报表面同一把尺——只在这个合计点出现。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecapMoney {
    /// 付费礼物行数（gold 且 total_coin>0）。
    pub paid_gifts: i64,
    /// 付费礼物折元总额。
    pub gift_yuan: f64,
    /// SC 行数 / 折元总额（SC price 单位即元，无换算）。
    pub sc_count: i64,
    pub sc_yuan: f64,
    /// 上舰接入行数（GUARD_BUY；上舰无可结算币轨，暂只计数）。
    pub guard_buys: i64,
    /// 上舰播报行数（USER_TOAST_V2，含续费播报，非付费事件本身）。
    pub toasts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapCard {
    /// ready = 有本场；empty = 零语料或零本场发言（诚实文案，不是报错）。
    pub status: String,
    pub generated_at: String,
    pub session: Option<RecapSession>,
    /// 规则一句话结论（程序事实直译，非 AI 语义）。
    pub headline: String,
    pub speakers: i64,
    pub returning: Option<RecapReturning>,
    pub peak: Option<RecapPeak>,
    pub repeated: Option<RecapRepeat>,
    /// AI 命名件：未命名 = null。呈现面须按缺位渲染，
    /// 不得把空命名补成看似完整的文案。
    pub naming: Option<RecapNaming>,
    /// 「未知的部分」行恒存在（至少空数组）。
    pub unknown: Vec<String>,
    /// 空场诚实文案（status=empty 才非空）。
    pub empty_copy: Option<String>,
    /// 金额面：当前场次窗金钱流水合计；零金钱事件 = null
    /// （呈现面按「本场零金钱流水」静默位渲染——0 也是事实，但不是错误）。
    pub money: Option<RecapMoney>,
}

/// 空场文案（细则原文句式 + Hamilton：0 同接也有价值——如实说，不报错）。
const EMPTY_COPY: &str = "今晚没能落下一句话——但这未必是你说错了话。\
     看一眼「未知的部分」：若是采集没碰到新场次，这页空白只是还没翻到，不是你不够好。";

fn unix_str_to_iso(raw: &Value) -> String {
    // B12：同 room_corpus——unix 解析与格式化沉入 episodes 公共区。
    crate::episodes::unix_secs_to_iso(crate::episodes::value_unix_secs(raw))
}

fn unix_to_i64(raw: &Value) -> Option<i64> {
    crate::episodes::value_unix_secs(raw)
}

/// 复读句归一化：trim + 折叠空白 + 全小写。CASE：规则层不做标点剥离
/// （「好耶！」≠「好耶」是平台原声的重现，剥离是认知层的事）。
pub fn normalize_sentence(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 身份键 = `轨|uid`。空 uid 恒为空串（不把「无身份行」升格成一个发言者）。
/// 跨轨不合并：同数字 uid 跨轨即不同键（冻结共识，杜绝 crc/mid 慢性认亲）。
fn track_identity(track: &str, uid: &str) -> String {
    if uid.is_empty() {
        String::new()
    } else {
        format!("{track}|{uid}")
    }
}

/// 场次发现的区间事实（回放束弹幕行 / WS 窗线沉降）。区间折叠的原子单位——
/// 只有客观时间事实（start/end + 源轨 + rid），不做任何语义判别。
#[derive(Debug)]
struct IntervalFact {
    start_ts: i64,
    end_ts: i64,
    /// 是否来自 WS 窗（主轨：折叠场上 WS 在场即 WS rid 优先）。
    is_ws: bool,
    rid: String,
}

#[derive(Debug)]
struct SessionRow {
    start_ts: i64,
    end_ts: i64,
    rid: String,
}

/// WS 窗元信息（起点诚实标记的读取侧；`closed_by` 已退出 facts contract——
/// 窗终局诚实面由 `WsWindowCapture` 承载，见模块头注）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct WsWindowMeta {
    start_ts: i64,
    end_ts: i64,
    window_start: String,
}

#[derive(Debug)]
struct LineRow {
    session_key: (i64, i64),
    /// 身份轨（replay/comment/ws）——跨轨不合并的归类锚。
    track: &'static str,
    /// 身份键（`轨|uid`，空 uid = 空串）。
    sender: String,
    ts: Option<i64>,
    text: String,
}

/// 金额面登记行：事实层原币轨照登（元换算只在合计点，见 RecapMoney 注释）。
#[derive(Debug)]
struct MoneyRow {
    /// 原场次窗（Episode facts 的 session 区段——归场规则与 LineRow 同尺）。
    start_ts: i64,
    end_ts: i64,
    kind: MoneyKind,
    /// SC/上舰 price（平台原值：SC=元）。
    price: f64,
    /// 礼物 total_coin（金瓜子）。
    total_coin: i64,
    /// coin_type=="gold"（silver 免费票不合计；原值照登记）。
    gold: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoneyKind {
    Gift,
    SuperChat,
    GuardBuy,
    Toast,
}

/// 区间折叠：重叠（严格相交 `next.start < cur.end`，边界相触不算）即并进同一场次；
/// 折叠界 = [min start, max end]。rid 遵循 WS 优先（is_ws 组件的 rid 随时重夺），
/// 否则取最早 start 的组件 rid。退化区间（start≤0 / end<start）不构成场次。
fn fold_intervals(facts: Vec<IntervalFact>) -> Vec<SessionRow> {
    let mut facts = facts;
    facts.sort_by_key(|f| (f.start_ts, f.end_ts));
    let mut out: Vec<SessionRow> = Vec::new();
    for fact in facts {
        if fact.start_ts <= 0 || fact.end_ts < fact.start_ts {
            continue;
        }
        match out.last_mut() {
            Some(last) if fact.start_ts < last.end_ts => {
                // 与末组件重叠 → 折叠。WS 在场即 WS rid 优先（重复组件不扰动）。
                if fact.is_ws {
                    last.rid = fact.rid;
                }
                last.end_ts = last.end_ts.max(fact.end_ts);
            }
            _ => out.push(SessionRow {
                start_ts: fact.start_ts,
                end_ts: fact.end_ts,
                rid: fact.rid,
            }),
        }
    }
    out
}

/// 规则聚四个数。`_room` 一条没有 → status=empty + 诚实文案；
/// 有场次但本场零发言 → status=empty + 诚实文案（session 字段仍给出本场窗）。
pub fn compute_recap(store: &Store) -> Result<RecapCard> {
    let rows =
        crate::graph::query::episodes(store, crate::episodes::room_corpus::ROOM_VIEWER_ID, None)?;
    let mut interval_facts: Vec<IntervalFact> = Vec::new();
    let mut line_rows: Vec<LineRow> = Vec::new();
    let mut money_rows: Vec<MoneyRow> = Vec::new();
    let mut comments: Vec<Value> = Vec::new();
    // WS 窗元信息（window_start 诚实标记）——折叠后按「当前场次窗」读取。
    let mut ws_windows: Vec<WsWindowMeta> = Vec::new();
    for row in &rows {
        // 读面键名：query::episodes 的 parse_json_field 把 *_json 后缀剥掉。
        let facts = row.get("platform_facts").cloned().unwrap_or(Value::Null);
        let text = row
            .get("fields")
            .and_then(Value::as_array)
            .and_then(|fields| {
                fields
                    .iter()
                    .find(|field| field.get("path").and_then(Value::as_str) == Some("text"))
            })
            .map(|field| {
                field
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default();
        match row.get("source").and_then(Value::as_str) {
            Some(crate::episodes::room_corpus::SOURCE_LIVE_DANMAKU) => {
                // 回放束：身份 = sender_uid_crc（crc32 轨——绝不用它充当真实 mid）。
                let start_ts = unix_to_i64(&facts["session"]["start_timestamp"]).unwrap_or(0);
                let end_ts = unix_to_i64(&facts["session"]["end_timestamp"]).unwrap_or(0);
                interval_facts.push(IntervalFact {
                    start_ts,
                    end_ts,
                    is_ws: false,
                    rid: facts
                        .get("rid")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
                line_rows.push(LineRow {
                    session_key: (start_ts, end_ts),
                    track: TRACK_REPLAY,
                    sender: track_identity(
                        TRACK_REPLAY,
                        facts
                            .get("sender_uid_crc")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    ),
                    ts: unix_to_i64(&facts["ts"]).filter(|ts| *ts > 0),
                    text,
                });
            }
            Some(crate::episodes::room_corpus::SOURCE_COMMENT) => {
                let ctime = unix_to_i64(&facts["ctime"]).unwrap_or(0);
                comments.push(serde_json::json!({
                    "mid": facts.get("mid").and_then(Value::as_str).unwrap_or(""),
                    "rpid": facts.get("rpid").and_then(Value::as_str).unwrap_or(""),
                    "ctime": ctime,
                    "text": text,
                }));
            }
            Some(source)
                if source == SOURCE_WS_DANMAKU
                    || source == SOURCE_WS_SC
                    || source == SOURCE_WS_ENTRY
                    || source == SOURCE_WS_GIFT
                    || source == SOURCE_WS_GUARD_BUY
                    || source == SOURCE_WS_TOAST =>
            {
                // 换轨：WS 窗的场次窗 = facts["session"]（窗线共享同一窗边界）；
                // 身份 = 真实 mid（`sender_uid_mid`，非回放 crc 轨）。
                let start_ts = unix_to_i64(&facts["session"]["start_timestamp"]).unwrap_or(0);
                let end_ts = unix_to_i64(&facts["session"]["end_timestamp"]).unwrap_or(0);
                let ws_rid = facts
                    .get("session")
                    .and_then(|s| s.get("rid"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let rid = if ws_rid.is_empty() {
                    format!("{SESSION_RID_PREFIX}:{start_ts}")
                } else {
                    ws_rid.to_string()
                };
                interval_facts.push(IntervalFact {
                    start_ts,
                    end_ts,
                    is_ws: true,
                    rid,
                });
                let meta = WsWindowMeta {
                    start_ts,
                    end_ts,
                    window_start: facts
                        .get("window_start")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                };
                if start_ts > 0 && end_ts > 0 && !ws_windows.contains(&meta) {
                    ws_windows.push(meta);
                }
                // 金额面登记：SC/礼物/上舰/播报——事实原值照登，换算权在合计点。
                let money_kind = match source {
                    s if s == SOURCE_WS_SC => Some(MoneyKind::SuperChat),
                    s if s == SOURCE_WS_GIFT => Some(MoneyKind::Gift),
                    s if s == SOURCE_WS_GUARD_BUY => Some(MoneyKind::GuardBuy),
                    s if s == SOURCE_WS_TOAST => Some(MoneyKind::Toast),
                    _ => None,
                };
                if let Some(kind) = money_kind {
                    money_rows.push(MoneyRow {
                        start_ts,
                        end_ts,
                        kind,
                        price: facts.get("price").and_then(Value::as_f64).unwrap_or(0.0),
                        total_coin: facts.get("total_coin").and_then(Value::as_i64).unwrap_or(0),
                        gold: facts.get("coin_type").and_then(Value::as_str) == Some("gold"),
                    });
                }
                if source == SOURCE_WS_DANMAKU || source == SOURCE_WS_SC {
                    // 弹幕/SC 行入 lines——SC 也是发言轨（含文本）；进场/礼物/上舰/
                    // 播报是纯氛围事实（无发言文本），不进四数行轨但场次窗照归。
                    line_rows.push(LineRow {
                        session_key: (start_ts, end_ts),
                        track: TRACK_WS,
                        sender: track_identity(
                            TRACK_WS,
                            facts
                                .get("sender_uid_mid")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                        ),
                        ts: unix_to_i64(&facts["ts"]).filter(|ts| *ts > 0),
                        text,
                    });
                }
            }
            _ => {}
        }
    }

    // 折叠：重叠时间区间并成一场（客观时间事实，绝不做语义合并）。
    // 重复组件（同窗多行）自然并入同一场，rid 由 WS 优先规则收敛，无需预去重。
    let mut sessions = fold_intervals(interval_facts);
    sessions.sort_by_key(|s| (s.end_ts, s.start_ts));

    let mut unknown: Vec<String> = Vec::new();
    if sessions.is_empty() {
        // 评论落了库但零场次窗可归 ≠ 「没碰到」——诚实分轨：
        // 数字照实给、空场原因照实说，四数不成立也绝不判无罪释放。
        if !comments.is_empty() {
            let ccount = comments.len();
            unknown.push(format!(
                "{ccount} 条评论在库但没有可判定的场次窗（零回放弹幕/场次失窗）——四个数不成立"
            ));
            let headline = format!(
                "采集落了 {ccount} 条评论，但零场次窗可归（无回放弹幕）——四个数不明；等一个有效场次再复盘。"
            );
            return Ok(RecapCard {
                status: "empty".to_string(),
                generated_at: now_iso(),
                session: None,
                headline,
                speakers: 0,
                returning: None,
                peak: None,
                repeated: None,
                naming: None,
                unknown,
                empty_copy: Some(
                    "有评论但零场次窗：这不是「没人来」，也不能判今晚有效——复盘等下一个有效场次。"
                        .to_string(),
                ),
                money: None,
            });
        }
        unknown.push("没有任何场次落进图里——采集没碰到新的回放弹幕/评论。".to_string());
        return Ok(RecapCard {
            status: "empty".to_string(),
            generated_at: now_iso(),
            session: None,
            headline: EMPTY_COPY.to_string(),
            speakers: 0,
            returning: None,
            peak: None,
            repeated: None,
            naming: None,
            unknown,
            empty_copy: Some(EMPTY_COPY.to_string()),
            money: None,
        });
    }

    // 行归属：各行按其原始场次窗区间内含于折叠场（折叠界是组件的并，必命中）；
    // 归属后行键统一换成折叠场界，后续 current/prior 判定共用同一套场对象。
    let mut lines: Vec<LineRow> = Vec::new();
    for mut line in line_rows {
        if let Some(session) = sessions
            .iter()
            .find(|s| s.start_ts <= line.session_key.0 && line.session_key.1 <= s.end_ts)
        {
            line.session_key = (session.start_ts, session.end_ts);
            lines.push(line);
        }
    }

    // sessions 尚是常驻槽位——取最新场的拷贝远离借用纠缠（SessionRow 体积极小：i64×2+String）。
    let current = SessionRow {
        start_ts: sessions.last().expect("non-empty").start_ts,
        end_ts: sessions.last().expect("non-empty").end_ts,
        rid: sessions.last().expect("non-empty").rid.clone(),
    };
    let prior: Vec<&SessionRow> = sessions
        .iter()
        .rev()
        .skip(1)
        .take(RECAP_LOOKBACK_SESSIONS)
        .collect();
    let current_key = (current.start_ts, current.end_ts);

    // 金额面：金钱事件按 LineRow 同尺归场；只合计当前场（最新场次窗）。
    // 零金钱事件 = None（面板静默位——0 与缺席要分开诚实）。
    let mut money_acc = RecapMoney {
        paid_gifts: 0,
        gift_yuan: 0.0,
        sc_count: 0,
        sc_yuan: 0.0,
        guard_buys: 0,
        toasts: 0,
    };
    for row in &money_rows {
        let owner = sessions
            .iter()
            .find(|s| s.start_ts <= row.start_ts && row.end_ts <= s.end_ts)
            .map(|s| (s.start_ts, s.end_ts));
        if owner != Some(current_key) {
            continue;
        }
        match row.kind {
            MoneyKind::Gift => {
                if row.gold && row.total_coin > 0 {
                    money_acc.paid_gifts += 1;
                    money_acc.gift_yuan += row.total_coin as f64 / 1000.0;
                }
            }
            MoneyKind::SuperChat => {
                if row.price > 0.0 {
                    money_acc.sc_count += 1;
                    money_acc.sc_yuan += row.price;
                }
            }
            MoneyKind::GuardBuy => money_acc.guard_buys += 1,
            MoneyKind::Toast => money_acc.toasts += 1,
        }
    }
    let money = if money_acc.paid_gifts == 0
        && money_acc.sc_count == 0
        && money_acc.guard_buys == 0
        && money_acc.toasts == 0
    {
        None
    } else {
        Some(money_acc)
    };

    // 换轨诚实面：当前场次若含 WS 窗且起点为 attach（未收 LIVE 校正），
    // 诚实标记进未知行（绝不补段、绝不假装起点=开播）。
    if let Some(meta) = ws_windows
        .iter()
        .find(|meta| current.start_ts <= meta.start_ts && meta.end_ts <= current.end_ts)
        && meta.window_start == WINDOW_START_ATTACH
    {
        unknown.push(
            "本场窗起点为 WS 附着时刻（window_start=attach）——未收到该场 LIVE 校正，\
             场次起点存在部分估计。"
                .to_string(),
        );
    }

    // 评论按 ctime 落窗归场（细则：comment 用 ctime 定场次窗；归属到折叠界）。
    let mut out_of_window_ctimes: Vec<i64> = Vec::new();
    for comment in &comments {
        let ctime = comment["ctime"].as_i64().unwrap_or(0);
        if ctime <= 0 {
            unknown.push(format!(
                "评论缺 ctime，无法归场：rpid={}",
                comment["rpid"].as_str().unwrap_or("")
            ));
            continue;
        }
        let owner = sessions
            .iter()
            .find(|s| s.start_ts <= ctime && ctime <= s.end_ts)
            .map(|s| (s.start_ts, s.end_ts));
        match owner {
            Some(key) => lines.push(LineRow {
                session_key: key,
                track: TRACK_COMMENT,
                sender: track_identity(TRACK_COMMENT, comment["mid"].as_str().unwrap_or("")),
                ts: Some(ctime),
                text: comment["text"].as_str().unwrap_or("").to_string(),
            }),
            None => {
                out_of_window_ctimes.push(ctime);
            }
        }
    }

    if !out_of_window_ctimes.is_empty() {
        out_of_window_ctimes.sort_unstable();
        let count = out_of_window_ctimes.len();
        let (first, last) = (out_of_window_ctimes[0], out_of_window_ctimes[count - 1]);
        unknown.push(format!(
            "{count} 条评论落在所有场次窗外（ctime 最早 {first}、最晚 {last}），未计入四个数"
        ));
    }

    let current_lines: Vec<&LineRow> = lines
        .iter()
        .filter(|line| line.session_key == current_key)
        .collect();
    let current_speakers: HashSet<&str> = current_lines
        .iter()
        .map(|line| line.sender.as_str())
        .filter(|sender| !sender.is_empty())
        .collect();
    let speakers = current_speakers.len() as i64;

    // 换轨诚实面：身份键跨轨不合并——本场发言多轨并出场时逐轨人数明示
    // （replay/ws/comment 三轨两两或三三同场），绝不假装是同一套 uid 的并集。
    let mut rail_speakers: Vec<(&str, usize)> = Vec::new();
    for (track, label) in [
        (TRACK_REPLAY, "弹幕"),
        (TRACK_WS, "WS 弹幕"),
        (TRACK_COMMENT, "评论"),
    ] {
        let count = current_lines
            .iter()
            .filter(|line| line.track == track)
            .map(|line| line.sender.as_str())
            .filter(|sender| !sender.is_empty())
            .collect::<HashSet<_>>()
            .len();
        if count > 0 {
            rail_speakers.push((label, count));
        }
    }
    if rail_speakers.len() >= 2 {
        let num_word = if rail_speakers.len() == 2 {
            "两条轨并出场"
        } else {
            "三条轨并出场"
        };
        let parts = rail_speakers
            .iter()
            .map(|(label, count)| format!("{label} {count} 人"))
            .collect::<Vec<_>>()
            .join(" + ");
        unknown.push(format!(
            "本场发言{num_word}：{parts}（跨轨不合并，人数按各自轨呈现）"
        ));
    }

    if speakers == 0 {
        unknown.push("本场（最新场次窗）零发言。".to_string());
        return Ok(RecapCard {
            status: "empty".to_string(),
            generated_at: now_iso(),
            session: Some(RecapSession {
                start: unix_str_to_iso(&Value::from(current.start_ts)),
                end: unix_str_to_iso(&Value::from(current.end_ts)),
                rid: current.rid.clone(),
            }),
            headline: "今晚没人来——但你没说错话。".to_string(),
            speakers: 0,
            returning: None,
            peak: None,
            repeated: None,
            naming: None,
            unknown,
            empty_copy: Some("今晚没人来——但你没说错话。".to_string()),
            money,
        });
    }

    // ① 回来过的：本场发言者 ∩ 前 N 场任意场发言者（身份键跨轨不合并——
    // 回放 crc 轨与 WS 真实 mid 轨是两套身份，绝不跨轨认亲）。
    let prior_keys: HashSet<(i64, i64)> = prior.iter().map(|s| (s.start_ts, s.end_ts)).collect();
    let prior_speakers: HashSet<&str> = lines
        .iter()
        .filter(|line| prior_keys.contains(&line.session_key))
        .map(|line| line.sender.as_str())
        .filter(|sender| !sender.is_empty())
        .collect();
    let returning = if prior.is_empty() {
        unknown.push("这是图里的第一场——「回来过的」无从算起，不做臆测。".to_string());
        None
    } else {
        Some(RecapReturning {
            count: current_speakers.intersection(&prior_speakers).count() as i64,
            base: speakers,
            sessions_back: prior.len() as i64,
        })
    };

    // ② 密度峰：10 分钟滑窗（只数弹幕行——回放+WS 都是弹幕行；评论只有 ctime
    // 颗粒，混进会稀释口径）。
    let mut timed: Vec<i64> = current_lines
        .iter()
        .filter(|line| line.track != TRACK_COMMENT)
        .filter_map(|line| line.ts)
        .collect();
    let danmaku_total = current_lines
        .iter()
        .filter(|line| line.track != TRACK_COMMENT)
        .count();
    let peak = if timed.is_empty() {
        if danmaku_total > 0 {
            unknown.push(format!(
                "本场 {danmaku_total} 行弹幕全部缺时间戳——密度峰无法定位"
            ));
        }
        None
    } else {
        timed.sort_unstable();
        let window = RECAP_PEAK_WINDOW_MINUTES * 60;
        let mut best: (i64, i64) = (timed[0], 0);
        let mut right = 0;
        for (left, ts) in timed.iter().enumerate() {
            while right < timed.len() && timed[right] < ts + window {
                right += 1;
            }
            let count = (right - left) as i64;
            if count > best.1 {
                best = (*ts, count);
            }
        }
        if timed.len() < danmaku_total {
            unknown.push(format!(
                "本场 {danmaku_total} 行弹幕里只有 {} 行带时间戳——峰按带戳部分计算",
                timed.len()
            ));
        }
        Some(RecapPeak {
            start: unix_str_to_iso(&Value::from(best.0)),
            count: best.1,
            window_minutes: RECAP_PEAK_WINDOW_MINUTES,
        })
    };

    // ③ 复读句（本场全部发言：弹幕+评论，归一化文本 ≥3 次 top-1）。
    let mut counter: HashMap<String, (i64, String)> = HashMap::new();
    for line in &current_lines {
        let raw = line.text.trim();
        if raw.is_empty() {
            continue;
        }
        let key = normalize_sentence(raw);
        let entry = counter.entry(key).or_insert_with(|| (0, raw.to_string()));
        entry.0 += 1;
    }
    let repeated = counter
        .values()
        .filter(|(count, _)| *count >= RECAP_REPEAT_MIN_COUNT)
        .max_by_key(|(count, _)| *count)
        .map(|(count, text)| RecapRepeat {
            text: text.clone(),
            count: *count,
        });

    let returning_line = match &returning {
        Some(ret) => format!(
            "，{} 人回来过（前 {} 场见过他们）",
            ret.count, ret.sessions_back
        ),
        None => "，首场没有「回来过」可算".to_string(),
    };
    let peak_line = match &peak {
        Some(p) => format!("；最密的十分钟有 {} 行弹幕", p.count),
        None => String::new(),
    };
    let repeated_line = match &repeated {
        Some(rep) => format!("；「{}」被刷了 {} 次", rep.text, rep.count),
        None => String::new(),
    };

    Ok(RecapCard {
        status: "ready".to_string(),
        generated_at: now_iso(),
        session: Some(RecapSession {
            start: unix_str_to_iso(&Value::from(current.start_ts)),
            end: unix_str_to_iso(&Value::from(current.end_ts)),
            rid: current.rid.clone(),
        }),
        headline: format!("今晚 {speakers} 人来过{returning_line}{peak_line}{repeated_line}"),
        speakers,
        returning,
        peak,
        repeated,
        naming: None,
        unknown,
        empty_copy: None,
        money,
    })
}

// ---------------------------------------------------------------------------
// 复盘解耦：T0 出卡口——collect 尾与 pipeline 尾共用。四个数是语料
// 纯规则（零 AI）；AI 命名只作「留存判定」：同场次同数面旧命名继续有效，
// 数面一动即显式作废（命名本身仍只在 pipeline 尾的 AI 窗里新跑）。
// ---------------------------------------------------------------------------

fn recap_card_path(output_dir: &Path) -> std::path::PathBuf {
    output_dir.join("ai").join("recap.json")
}

/// 旧卡读面——磁盘工件是命名的唯一载体，不留进程内暗账。
fn read_previous_card(output_dir: &Path) -> Option<RecapCard> {
    crate::storage::read_json(&recap_card_path(output_dir))
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_value::<RecapCard>(value).ok())
}

/// 命名有效性口径：场次 + 四个数（status/speakers/returning/peak/repeated）全等。
fn fact_surface(card: &RecapCard) -> Value {
    serde_json::to_value((
        &card.status,
        &card.session,
        card.speakers,
        &card.returning,
        &card.peak,
        &card.repeated,
    ))
    .unwrap_or(Value::Null)
}

/// 复盘卡落盘点（ai/recap.json）——refresh / pipeline 命名路径共用的唯一写门。
pub fn write_recap_card(output_dir: &Path, card: &RecapCard) -> Result<()> {
    crate::storage::write_json(
        &recap_card_path(output_dir),
        &serde_json::to_value(card).unwrap_or(Value::Null),
    )
    .map_err(|err| StoreError::Repo(format!("recap 落盘：{err}")))?;
    Ok(())
}

/// 语料入账（幂等）→ 聚四个数 → 留存/作废旧命名 → 落盘。返回新卡——pipeline 尾
/// 在其上叠 AI 命名窗（见 agent/pipeline.rs 尾部）。
///
/// 失败纪律：store 开账/规则计算失败 = Err（调用方响铃）；语料入账失败 = 响铃 +
/// fail_run 记半账 + 按图里已有面出卡——卡是呈现层读物，一条缺账不放弃四个数。
pub fn refresh_recap_card(output_dir: &Path, progress: &dyn Fn(&str)) -> Result<RecapCard> {
    let store = Store::open(&output_dir.join("graph").join("perception.sqlite3"))?;
    // 入账挂独立 run（kind=recap-refresh）：completed 照常记账，但
    // run_pair_delta 显式排除（对照窗语义见 query.rs 头注）。
    let run_id = format!("run:{}", uuid::Uuid::new_v4().simple());
    store.begin_run_typed(
        &run_id,
        &now_iso(),
        "recap-refresh",
        Store::RUN_KIND_RECAP_REFRESH,
        None,
    )?;
    let (corpus, counts) =
        crate::episodes::room_corpus::room_corpus_episodes(&output_dir.join("shared"));
    let danmaku_count = counts["live_danmaku"].as_i64().unwrap_or(0);
    let comment_count = counts["room_comment"].as_i64().unwrap_or(0);
    let mut ingest_note: Option<String> = None;
    match crate::episodes::room_corpus::ingest_room_corpus(&store, &run_id, &corpus) {
        Ok(()) => {
            progress(&format!(
                "[RECAP] 房间语料入账：弹幕 {danmaku_count} 行、评论 {comment_count} 条（_room 命名空间）"
            ));
            if let Err(err) = store.complete_run(&run_id) {
                progress(&format!("[RECAP] 刷新 run 结清失败：{err}"));
            }
        }
        Err(err) => {
            let note = format!(
                "房间语料入账失败：{err}（弹幕 {danmaku_count}、评论 {comment_count} 按图里已有面出卡）"
            );
            progress(&format!("[RECAP] {note}"));
            if let Err(close_err) = store.fail_run(&run_id, &err.to_string(), false) {
                progress(&format!("[RECAP] 刷新 run 记败失败：{close_err}"));
            }
            ingest_note = Some(note);
        }
    }
    let mut card = compute_recap(&store)?;
    if let Some(note) = ingest_note {
        card.unknown.push(note);
    }
    // 旧命名留存纪律：双方 ready 且场次+四个数全等 → 旧 AI 命名继续有效（同场次
    // 同数面不白跑 AI）；旧卡带命名而数面已动 → 显式作废 + 未知行（绝不盖新数）。
    if card.naming.is_none()
        && let Some(previous) = read_previous_card(output_dir)
        && previous.naming.is_some()
    {
        if fact_surface(&previous) == fact_surface(&card) {
            card.naming = previous.naming;
        } else if previous.status == "ready" {
            card.unknown
                .push("四个数较上次命名时有变化——旧 AI 命名已作废，下一轮感知重新命名".to_string());
        }
    }
    write_recap_card(output_dir, &card)?;
    progress(&format!("[RECAP] 复盘卡落盘（status={}）", card.status));
    Ok(card)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::episodes::room_corpus::{
        comment_to_episode, danmaku_to_episode, ingest_room_corpus,
    };

    /// 场次日程表形：((开播秒, 下播秒), [(发话秒, 文本, 发话人)])。
    type SessionFixture<'a> = ((i64, i64), Vec<(i64, &'a str, &'a str)>);

    fn store_with_corpus(sessions: &[SessionFixture<'_>]) -> Store {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.sqlite3");
        let store = Store::open(&path).expect("store opens");
        store
            .begin_run_fixed("run-recap", &now_iso(), "test")
            .expect("begin run");
        let mut episodes = Vec::new();
        for (index, ((start, end), lines)) in sessions.iter().enumerate() {
            for (line_index, (ts, text, uid)) in lines.iter().enumerate() {
                let row = json!({"text": text, "uid": uid, "shard_index": 0, "ts": ts});
                episodes.push(danmaku_to_episode(
                    &format!("rid-{index}"),
                    &json!(start),
                    &json!(end),
                    0,
                    line_index as i64,
                    &row,
                    "obs",
                ));
            }
        }
        ingest_room_corpus(&store, "run-recap", &episodes).expect("ingest");
        store
    }

    /// 钉子①：合成场景（3 人夜）四个数人工可复算。
    /// 场次：S1=[1000,2000]，S2=[3000,4600]（本场）。
    /// S2 弹幕：A×2（3020,3030）B×1（4000）C×3（4200,4210,4300 同句「晚上好！」）。
    /// → 发言 3 人；A 在 S1 也说过 → 回来 1/3；
    ///   峰：4000 起手 10 分窗 [4000,4600) 含 4000/4200/4210/4300 四行 → top=4@4000；
    ///   复读句「晚上好！」归一化 3 次 ≥3 → top-1。
    #[test]
    fn three_people_night_all_four_numbers_hand_checkable() {
        let store = store_with_corpus(&[
            ((1000, 2000), vec![(1100, "旧场", "A")]),
            (
                (3000, 4600),
                vec![
                    (3020, "晚上好！", "C"),
                    (3030, "来了", "B"),
                    (4000, "唱的不错", "B"),
                    (4200, "晚上好！", "C"),
                    (4210, "晚上好！", "C"),
                    (4300, "好吧", "A"),
                ],
            ),
        ]);
        let card = compute_recap(&store).expect("recap");
        assert_eq!(card.status, "ready");
        assert_eq!(card.speakers, 3, "A/B/C 三人在本场说过话");
        let returning = card.returning.expect("prior sessions exist");
        assert_eq!((returning.count, returning.base), (1, 3));
        assert_eq!(returning.sessions_back, 1);
        let peak = card.peak.expect("ts present");
        assert_eq!(
            peak.count, 4,
            "4000 起手 10 分窗 [4000,4600): 4000/4200/4210/4300 四行"
        );
        assert_eq!(peak.start, unix_str_to_iso(&json!(4000)));
        assert_eq!(peak.window_minutes, RECAP_PEAK_WINDOW_MINUTES);
        let repeated = card.repeated.expect("3 次复读句存在");
        assert_eq!(repeated.text, "晚上好！");
        assert_eq!(repeated.count, 3);
        assert!(card.naming.is_none(), "AI 命名件缺位是 null，不是伪造");
        assert!(
            card.unknown.is_empty(),
            "干净场景未知行为空: {:?}",
            card.unknown
        );
        assert!(card.headline.contains("3 人来过") && card.headline.contains("1 人回来过"));
    }

    /// 钉子②：空场形态——零 _room 时诚实文案 + 未知行，不报错。
    #[test]
    fn empty_room_is_honest_copy_not_error() {
        let store = store_with_corpus(&[]);
        let card = compute_recap(&store).expect("empty must not err");
        assert_eq!(card.status, "empty");
        assert_eq!(card.empty_copy.as_deref(), Some(EMPTY_COPY));
        assert!(!card.unknown.is_empty(), "未知行恒存在");
        assert!(card.naming.is_none());
    }

    /// 空场的姊妹形态：场次由弹幕行反推——单人单场无历史场，「回来过」=None+未知行。
    #[test]
    fn session_without_lines_is_honest_empty() {
        let store = store_with_corpus(&[((1000, 2000), vec![(1100, "hi", "A")])]);
        let card = compute_recap(&store).expect("recap");
        assert_eq!(card.status, "ready");
        assert_eq!(card.speakers, 1);
        assert!(card.returning.is_none());
        assert!(
            card.unknown.iter().any(|row| row.contains("第一场")),
            "无历史场要进未知行: {:?}",
            card.unknown
        );
    }

    /// 缺时间戳：峰 None + 未知行（诚实面，绝非回退到行序）。
    #[test]
    fn missing_ts_drops_peak_into_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("g.sqlite3")).unwrap();
        store.begin_run_fixed("r", &now_iso(), "t").unwrap();
        let episodes = vec![
            danmaku_to_episode(
                "rid-0",
                &json!(1000),
                &json!(2000),
                0,
                0,
                &json!({"text":"a","uid":"A","shard_index":0}),
                "o",
            ),
            danmaku_to_episode(
                "rid-0",
                &json!(1000),
                &json!(2000),
                0,
                1,
                &json!({"text":"b","uid":"B","shard_index":0}),
                "o",
            ),
        ];
        ingest_room_corpus(&store, "r", &episodes).unwrap();
        let card = compute_recap(&store).unwrap();
        assert!(card.peak.is_none());
        assert!(card.unknown.iter().any(|row| row.contains("缺时间戳")));
    }

    #[test]
    fn comment_joins_session_by_ctime() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("g.sqlite3")).unwrap();
        store.begin_run_fixed("r", &now_iso(), "t").unwrap();
        let dm = danmaku_to_episode(
            "rid-0",
            &json!(1000),
            &json!(2000),
            0,
            0,
            &json!({"text":"a","uid":"A","shard_index":0,"ts":1100}),
            "o",
        );
        let inside = comment_to_episode(
            &json!({"rpid":"1","mid":"M","message":"来力","ctime":"1500"}),
            "o",
        )
        .unwrap();
        let outside = comment_to_episode(
            &json!({"rpid":"2","mid":"N","message":"旧评","ctime":"500"}),
            "o",
        )
        .unwrap();
        ingest_room_corpus(&store, "r", &[dm, inside, outside]).unwrap();
        let card = compute_recap(&store).unwrap();
        assert_eq!(card.speakers, 2, "A+M；窗外评论不计入");
        assert!(card.unknown.iter().any(|row| row.contains("窗外")));
        assert_eq!(card.peak.unwrap().count, 1, "评论不混进弹幕密度口径");
    }
}

#[cfg(test)]
mod round2_tests {
    use serde_json::json;

    use super::*;
    use crate::episodes::room_corpus::{
        comment_to_episode, danmaku_to_episode, ingest_room_corpus,
    };

    fn fresh_store(tag: &str) -> Store {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join(format!("g-{tag}.sqlite3"))).unwrap();
        store.begin_run_fixed("r", &now_iso(), "t").unwrap();
        store
    }

    /// 弹幕+评论同场都有人 → 未知行必须明示身份轨多轨并出场
    /// （冻结共识：replay|crc 与 comment|mid 是两套身份轨，**跨轨不合并**）。
    #[test]
    fn mixed_kinds_raise_identity_risk_row() {
        let store = fresh_store("mix");
        let dm = danmaku_to_episode(
            "rid-m",
            &json!(1000),
            &json!(2000),
            0,
            0,
            &json!({"text":"好","uid":"1001","shard_index":0,"ts":1100}),
            "o",
        );
        let cm = comment_to_episode(
            &json!({"rpid":"9","mid":"1001","message":"同上","ctime":"1200"}),
            "o",
        )
        .unwrap();
        ingest_room_corpus(&store, "r", &[dm, cm]).unwrap();
        let card = compute_recap(&store).unwrap();
        assert_eq!(
            card.speakers, 2,
            "跨轨不合并：replay|1001 与 comment|1001 是两套身份键，按两轨计"
        );
        assert!(
            card.unknown.iter().any(|row| row.contains("两条轨并出场")
                && row.contains("弹幕")
                && row.contains("评论")),
            "身份双轨必须明示入未知行: {:?}",
            card.unknown
        );
    }

    /// 零弹幕但只有评论 —— 诚实形态：文案必须承认「落了 N 条评论、
    /// 零场次窗可归」，不许说「没碰到」。
    #[test]
    fn comments_without_sessions_are_honestly_named_not_dropped() {
        let store = fresh_store("conly");
        let cm1 = comment_to_episode(
            &json!({"rpid":"9","mid":"1001","message":"早","ctime":"1200"}),
            "o",
        )
        .unwrap();
        let cm2 = comment_to_episode(
            &json!({"rpid":"10","mid":"1002","message":"晚","ctime":"1300"}),
            "o",
        )
        .unwrap();
        ingest_room_corpus(&store, "r", &[cm1, cm2]).unwrap();
        let card = compute_recap(&store).unwrap();
        assert_eq!(card.status, "empty");
        assert!(
            card.headline.contains("评论") && card.headline.contains("2"),
            "诚实文案必须承认评论数字: {}",
            card.headline
        );
        assert!(
            !card.headline.contains("没碰到"),
            "旧文案=谎话: {}",
            card.headline
        );
    }
}

/// 换轨钉组：WS 窗进复盘——区间折叠（重叠即并场）、WS rid 优先、
/// `轨|uid` 身份跨轨不合并、attach 起点诚实未知行。
#[cfg(test)]
mod ws_recap_tests {
    use serde_json::json;

    use super::*;
    use crate::episodes::room_corpus::{ingest_room_corpus, replay_danmaku_episodes};
    use crate::live_ws::episodes::{WsRecorder, ingest_ws_window};
    use crate::live_ws::message::WsEvent;
    use crate::live_ws::session::SessionEnd;

    fn fresh_store(tag: &str) -> Store {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join(format!("g-{tag}.sqlite3"))).unwrap();
        store.begin_run_fixed("r", &now_iso(), "t").unwrap();
        store
    }

    fn assert_unknown_has(card: &RecapCard, needle: &str) {
        assert!(
            card.unknown.iter().any(|row| row.contains(needle)),
            "未知行缺少「{needle}」: {:?}",
            card.unknown
        );
    }

    /// 金额面钉：同窗 弹幕+付费礼物(gold)+免费票(silver)+SC+上舰+播报 ——
    /// 合计只计当前场；gold 才折元（total_coin/1000）、免费票剔；上舰/播报只计数。
    /// 另钉零金钱窗 → money=None（0 与缺席分轨诚实）。
    #[test]
    fn money_face_totals_current_session_and_none_when_zero() {
        let store = fresh_store("money");
        let mut rec = WsRecorder::attach(7, 1_000_000, 1).expect("在播开窗");
        let feed = |rec: &mut WsRecorder, ev: WsEvent, ts: i64| rec.on_event(&ev, ts);
        feed(
            &mut rec,
            WsEvent::Danmaku {
                uid: "u-1".into(),
                uname: "甲".into(),
                text: "好".into(),
                ts: Some(1_000_001),
            },
            1_000_001,
        );
        feed(
            &mut rec,
            WsEvent::Gift {
                uid: "u-2".into(),
                uname: "乙".into(),
                gift_name: "小花花".into(),
                num: 2,
                price: 100.0,
                total_coin: 200,
                coin_type: "gold".into(),
                ts: Some(1_000_002),
            },
            1_000_002,
        );
        feed(
            &mut rec,
            WsEvent::Gift {
                uid: "u-3".into(),
                uname: "丙".into(),
                gift_name: "辣条".into(),
                num: 1,
                price: 0.0,
                total_coin: 0,
                coin_type: "silver".into(),
                ts: Some(1_000_003),
            },
            1_000_003,
        );
        feed(
            &mut rec,
            WsEvent::SuperChat {
                uid: "u-4".into(),
                uname: "金主".into(),
                text: "大气".into(),
                price: 30.0,
                start_time: Some(1_000_004),
            },
            1_000_004,
        );
        feed(
            &mut rec,
            WsEvent::GuardBuy {
                uid: "u-5".into(),
                uname: "舰长".into(),
                guard_level: 3,
                gift_name: "舰长".into(),
                num: 1,
                price: 198.0,
                ts: Some(1_000_005),
            },
            1_000_005,
        );
        feed(
            &mut rec,
            WsEvent::Toast {
                uid: "u-5".into(),
                uname: "舰长".into(),
                role_name: "舰长".into(),
                guard_level: 3,
                ts: Some(1_000_006),
            },
            1_000_006,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_007);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_007);
        ingest_ws_window(&store, "r", &cap).unwrap();

        let card = compute_recap(&store).unwrap();
        let money = card.money.expect("金钱事件齐了必出金额面");
        assert_eq!(money.paid_gifts, 1, "silver 免费票不计付费");
        assert_eq!(money.gift_yuan, 0.2, "200 金瓜子 = ¥0.2");
        assert_eq!(money.sc_count, 1);
        assert_eq!(money.sc_yuan, 30.0);
        assert_eq!(money.guard_buys, 1);
        assert_eq!(money.toasts, 1);

        // 零金钱场：同店再来一张纯弹幕窗 → money 缺席（None 与 0 分轨）。
        let store2 = fresh_store("moneyzero");
        let mut rec2 = WsRecorder::attach(7, 2_000_000, 1).expect("在播开窗");
        rec2.on_event(
            &WsEvent::Danmaku {
                uid: "u-1".into(),
                uname: "甲".into(),
                text: "在".into(),
                ts: Some(2_000_001),
            },
            2_000_001,
        );
        rec2.on_event(&WsEvent::Preparing { round: 1 }, 2_000_002);
        let cap2 = rec2.finish(&SessionEnd::Closed, 2_000_002);
        store2.begin_run_fixed("r2", &now_iso(), "t").unwrap();
        ingest_ws_window(&store2, "r2", &cap2).unwrap();
        let card2 = compute_recap(&store2).unwrap();
        assert!(
            card2.money.is_none(),
            "零金钱事件窗 → None: {:?}",
            card2.money
        );

        // 跨场不串账：首场金钱窗不动，第二场对店员合一——当前场合计只看最新场。
        let mut rec3 = WsRecorder::attach(7, 3_000_000, 1).expect("在播开窗");
        rec3.on_event(
            &WsEvent::Danmaku {
                uid: "u-9".into(),
                uname: "壬".into(),
                text: "晚".into(),
                ts: Some(3_000_001),
            },
            3_000_001,
        );
        rec3.on_event(&WsEvent::Preparing { round: 1 }, 3_000_002);
        let cap3 = rec3.finish(&SessionEnd::Closed, 3_000_002);
        store.begin_run_fixed("r3", &now_iso(), "t").unwrap();
        ingest_ws_window(&store, "r3", &cap3).unwrap();
        let card3 = compute_recap(&store).unwrap();
        assert!(
            card3.money.is_none(),
            "最新场零金钱 → None（首场的 ¥30.2 不得串入）: {:?}",
            card3.money
        );
    }

    /// 合成 WS 语料 → ready 卡：一窗弹幕+SC（LIVE 校正起点）直接出四数，无需任何
    /// 回放束；rid 走 WS 优先（`ws:{start}`）。
    #[test]
    fn synthetic_ws_corpus_ready_card() {
        let store = fresh_store("ws1");
        let mut rec = WsRecorder::attach(7, 1_000_000, 1).expect("在播开窗");
        rec.on_event(&WsEvent::Live { live_time: 999_000 }, 1_000_001);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u-1001".into(),
                uname: "Ａ".into(),
                text: "好耶".into(),
                ts: None,
            },
            1_000_002,
        );
        rec.on_event(
            &WsEvent::SuperChat {
                uid: "u-2002".into(),
                uname: "Ｂ".into(),
                text: "老板大气".into(),
                price: 30.0,
                start_time: Some(1_000_003),
            },
            1_000_003,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_004);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_004);
        ingest_ws_window(&store, "r", &cap).unwrap();

        let card = compute_recap(&store).unwrap();
        assert_eq!(card.status, "ready");
        assert_eq!(card.speakers, 2, "WS 弹幕+SC 两人（真实 mid 身份轨）");
        let session = card.session.expect("session");
        assert_eq!(session.rid, "ws:999000", "WS 窗 rid 优先");
        assert_eq!(
            session.start,
            unix_str_to_iso(&json!(999_000)),
            "LIVE 校正全场起点"
        );
        assert_eq!(card.peak.unwrap().count, 2, "弹幕+SC 两行带 ts 都算密度");
        assert!(
            !card
                .unknown
                .iter()
                .any(|row| row.contains("window_start=attach")),
            "LIVE 校正后不出现 attach 未知行: {:?}",
            card.unknown
        );
    }

    /// 混合轨折叠：回放束与 WS 窗同夜同窗（重叠）→ 折叠成一场（不翻倍场次）；
    /// rid 走 WS 优先；身份跨轨不合并（回放 2 + WS 2 = 4 人），双轨并出场进未知行。
    #[test]
    fn mixed_track_window_folds_into_one_session() {
        let store = fresh_store("fold");
        // 回放束窗外 [1000,2000]：两行（sender_uid_crc 轨）。
        let replay = replay_danmaku_episodes(
            &json!({"records":[{
            "rid":"r1","start_timestamp":1000,"end_timestamp":2000,
            "messages":[
                {"text":"旧辑A","uid":"999","shard_index":0,"ts":1100},
                {"text":"旧辑B","uid":"888","shard_index":0,"ts":1200},
            ]}]}),
            "obs",
        );
        ingest_room_corpus(&store, "r", &replay).unwrap();
        // WS 窗同窗 [1000,2000]（LIVE 校正 → window_start=live）：两行。
        let mut rec = WsRecorder::attach(7, 1000, 1).unwrap();
        rec.on_event(&WsEvent::Live { live_time: 1000 }, 1000);
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u1".into(),
                uname: "Ａ".into(),
                text: "好耶".into(),
                ts: None,
            },
            1100,
        );
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u2".into(),
                uname: "Ｂ".into(),
                text: "来了".into(),
                ts: None,
            },
            1200,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 2000);
        let ws_eps = rec.finish(&SessionEnd::Closed, 2000);
        ingest_ws_window(&store, "r", &ws_eps).unwrap();

        let card = compute_recap(&store).unwrap();
        assert_eq!(card.status, "ready");
        assert_eq!(
            card.speakers, 4,
            "跨轨不合并：replay|999/replay|888 + ws|u1/ws|u2 = 4 人"
        );
        let session = card.session.expect("session");
        assert_eq!(session.rid, "ws:1000", "WS rid 优先于回放 r1");
        let row = card
            .unknown
            .iter()
            .find(|row| row.contains("两条轨并出场"))
            .expect("双轨并出场必须进未知行");
        assert!(
            row.contains("弹幕") && row.contains("WS 弹幕"),
            "回放弹幕 + WS 弹幕双轨明示: {row}"
        );
    }

    /// attach 起点（未收 LIVE）→ 诚实未知行；起点=附着时刻。
    #[test]
    fn attach_window_raises_honest_unknown() {
        let store = fresh_store("attach");
        let mut rec = WsRecorder::attach(7, 1_000_000, 1).unwrap();
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u-1".into(),
                uname: "Ａ".into(),
                text: "附着即播".into(),
                ts: None,
            },
            1_000_001,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 1_000_002);
        let cap = rec.finish(&SessionEnd::Closed, 1_000_002);
        ingest_ws_window(&store, "r", &cap).unwrap();
        let card = compute_recap(&store).unwrap();
        assert_eq!(card.session.clone().unwrap().rid, "ws:1000000");
        assert_unknown_has(&card, "window_start=attach");
    }

    /// 多项重叠折叠：两条部分重叠的回放束折叠成一场，折叠界 = [min start, max end]。
    #[test]
    fn overlapping_replay_windows_fold_to_union_bounds() {
        let store = fresh_store("overlap");
        let payload = json!({"records":[
            {"rid":"r1","start_timestamp":1000,"end_timestamp":2000,
             "messages":[{"text":"早","uid":"1","shard_index":0,"ts":1100}]},
            {"rid":"r2","start_timestamp":1500,"end_timestamp":2500,
             "messages":[{"text":"晚","uid":"2","shard_index":0,"ts":1600},
                         {"text":"夜","uid":"3","shard_index":0,"ts":1700}]},
        ]});
        let eps = replay_danmaku_episodes(&payload, "obs");
        ingest_room_corpus(&store, "r", &eps).unwrap();
        let card = compute_recap(&store).unwrap();
        assert_eq!(card.status, "ready");
        let session = card.session.expect("session");
        assert_eq!(
            session.start,
            unix_str_to_iso(&json!(1000)),
            "折叠界 = min start"
        );
        assert_eq!(
            session.end,
            unix_str_to_iso(&json!(2500)),
            "折叠界 = max end"
        );
        assert_eq!(card.speakers, 3, "三行入同一场");
        assert!(card.returning.is_none(), "仅一场 → 无历史场");
        assert!(
            card.unknown.iter().any(|row| row.contains("第一场")),
            "{:?}",
            card.unknown
        );
    }

    /// 进场-only 窗：场次窗有效（照归）但零发言——诚实空场带 session，不伪造。
    #[test]
    fn entry_only_window_is_honest_empty_with_session() {
        let store = fresh_store("entry");
        let mut rec = WsRecorder::attach(9, 5_000_000, 1).unwrap();
        rec.on_event(
            &WsEvent::Interact {
                kind: 1,
                uid: "3003".into(),
                uname: "Ｃ".into(),
                ts: 5_000_001,
            },
            5_000_001,
        );
        rec.on_event(&WsEvent::Preparing { round: 1 }, 5_000_002);
        let cap = rec.finish(&SessionEnd::Closed, 5_000_002);
        ingest_ws_window(&store, "r", &cap).unwrap();
        let card = compute_recap(&store).unwrap();
        assert_eq!(card.status, "empty", "进场不是发言——四数行轨零行");
        assert_eq!(card.speakers, 0);
        assert_eq!(card.session.clone().unwrap().rid, "ws:5000000");
        assert_unknown_has(&card, "零发言");
    }

    /// 换轨后的 WS 断连真相不在 graph facts（contract：WsWindowCapture 承载）——
    /// recap 不臆造；此钉锁「recap 读不到 closed_by 也不撒谎」。
    #[test]
    fn recap_does_not_fabricate_window_close_reason() {
        let store = fresh_store("noclosed");
        let mut rec = WsRecorder::attach(3, 1_000_000, 1).unwrap();
        rec.on_event(
            &WsEvent::Danmaku {
                uid: "u1".into(),
                uname: "Ａ".into(),
                text: "hi".into(),
                ts: None,
            },
            1_000_001,
        );
        let cap = rec.finish(&SessionEnd::ReconnectExhausted, 9_999_999);
        assert_eq!(
            cap.end_reason, "reconnect_exhausted",
            "终局真相在 capture 层"
        );
        ingest_ws_window(&store, "r", &cap).unwrap();
        let card = compute_recap(&store).unwrap();
        // 不伪造任何「断连/保险丝」未知行——那是 outcome 层的账，不在 facts。
        assert!(
            !card
                .unknown
                .iter()
                .any(|row| row.contains("保险丝") || row.contains("采录中断")),
            "recap 不臆造窗终局: {:?}",
            card.unknown
        );
    }
}

/// 钉组：refresh_recap_card 的纪律面值——零 AI 出卡 / refresh run 记账 /
/// 旧命名留存 / 数面动即作废。布景直接写 shared/*.json（真通道读面）。
#[cfg(test)]
mod refresh_tests {
    use serde_json::{Value, json};

    use super::*;

    fn seed_records(output_dir: &Path, records: Value) {
        let shared = output_dir.join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(
            shared.join("replay_danmaku.json"),
            json!({"records": records}).to_string(),
        )
        .unwrap();
    }

    fn one_session_records() -> Value {
        json!([{
            "rid": "r1", "start_timestamp": 1000, "end_timestamp": 2000,
            "messages": [
                {"text": "hi", "uid": "1001", "shard_index": 0, "ts": 1100},
                {"text": "yo", "uid": "1002", "shard_index": 0, "ts": 1200},
            ],
        }])
    }

    /// 人工垫上旧命名（等价于上一轮 AI 已命名落盘）。
    fn graft_old_naming(output_dir: &Path) {
        let path = recap_card_path(output_dir);
        let mut value = crate::storage::read_json(&path).unwrap().unwrap();
        value["naming"] = json!({
            "peak_name": "旧峰名", "sentence_name": "旧句名",
            "reuse_line": "旧复用句", "cut_advice": "旧切口", "named_at": "t0",
        });
        crate::storage::write_json(&path, &value).unwrap();
    }

    /// 钉子①：零 AI 出卡——磁盘只见 shared/ 语料，refresh 即落 ready 卡；
    /// refresh run 以 recap-refresh 类完整结账在案（kind + completed 双钉）。
    #[test]
    fn refresh_writes_card_with_zero_ai() {
        let dir = tempfile::tempdir().unwrap();
        seed_records(dir.path(), one_session_records());
        let card = refresh_recap_card(dir.path(), &|_| {}).expect("refresh");
        assert_eq!(card.status, "ready");
        assert_eq!(card.speakers, 2);
        assert!(card.naming.is_none(), "纯规则路径绝不伪造 AI 命名");
        let on_disk = crate::storage::read_json(&recap_card_path(dir.path()))
            .unwrap()
            .expect("card on disk");
        assert_eq!(on_disk["status"], "ready");
        let store = Store::open(&dir.path().join("graph").join("perception.sqlite3")).unwrap();
        assert_eq!(
            store
                .count_scalar(
                    "SELECT COUNT(*) FROM graph_runs \
                     WHERE kind='recap-refresh' AND completed_at IS NOT NULL",
                    &[]
                )
                .unwrap(),
            1,
            "refresh run 必须完整结账"
        );
    }

    /// 钉子②：同场次同数面 → 旧 AI 命名直接留存（懒惰语义锚：不白跑 AI）。
    #[test]
    fn refresh_preserves_naming_when_facts_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        seed_records(dir.path(), one_session_records());
        refresh_recap_card(dir.path(), &|_| {}).unwrap();
        graft_old_naming(dir.path());
        let card = refresh_recap_card(dir.path(), &|_| {}).unwrap();
        assert_eq!(card.naming.expect("旧命名留存").peak_name, "旧峰名");
        assert!(
            !card.unknown.iter().any(|row| row.contains("作废")),
            "数面未动不得作废旧命名: {:?}",
            card.unknown
        );
    }

    /// 钉子③：数面动了（新场次进场）→ 旧命名显式作废 + 未知行，绝不盖新数。
    #[test]
    fn refresh_drops_stale_naming_into_unknown() {
        let dir = tempfile::tempdir().unwrap();
        seed_records(dir.path(), one_session_records());
        refresh_recap_card(dir.path(), &|_| {}).unwrap();
        graft_old_naming(dir.path());
        // 第二场进场——本场窗从 [1000,2000] 换到 [3000,4600]。
        seed_records(
            dir.path(),
            json!([
                {
                    "rid": "r1", "start_timestamp": 1000, "end_timestamp": 2000,
                    "messages": [
                        {"text": "hi", "uid": "1001", "shard_index": 0, "ts": 1100},
                        {"text": "yo", "uid": "1002", "shard_index": 0, "ts": 1200},
                    ],
                },
                {
                    "rid": "r2", "start_timestamp": 3000, "end_timestamp": 4600,
                    "messages": [
                        {"text": "新场", "uid": "1003", "shard_index": 0, "ts": 3100},
                    ],
                },
            ]),
        );
        let card = refresh_recap_card(dir.path(), &|_| {}).unwrap();
        assert!(card.naming.is_none(), "数面动了旧命名必须作废");
        assert!(
            card.unknown.iter().any(|row| row.contains("已作废")),
            "作废必须进未知行: {:?}",
            card.unknown
        );
        // 落盘面同样作废——读到磁盘的卡不得残留旧命名。
        let on_disk = crate::storage::read_json(&recap_card_path(dir.path()))
            .unwrap()
            .unwrap();
        assert_eq!(on_disk["naming"], Value::Null);
    }

    /// 钉子④：入账通道缺席（连 shared/ 目录都没有）也是零语料空场卡，
    /// 不报错、未知行恒在。
    #[test]
    fn refresh_without_any_corpus_is_honest_empty() {
        let dir = tempfile::tempdir().unwrap();
        let card = refresh_recap_card(dir.path(), &|_| {}).unwrap();
        assert_eq!(card.status, "empty");
        assert!(card.empty_copy.is_some());
        assert!(!card.unknown.is_empty(), "未知行恒在");
    }
}
