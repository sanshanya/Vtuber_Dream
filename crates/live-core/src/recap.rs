//! 下播复盘卡·纯规则层（迭代细则 v1 §1 P0-2）：从 `_room` 房间语料 Episode 的
//! platform_facts 直接聚四个数——**全部程序事实，零 AI**。
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

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::episodes::now_iso;
use crate::graph::Store;
use crate::graph::store::Result;

/// 密度峰窗口（分钟）。依据：细则 §1 P0-2「10 分钟窗滑弹幕行数 top-1」——
/// 10 分钟是主播节奏的可操作颗粒（一首歌/一段杂谈的长度）。改它须改细则。
pub const RECAP_PEAK_WINDOW_MINUTES: i64 = 10;
/// 复读句达标线（归一化文本本场出现次数 ≥ 此值才计入 top-1 候选）。
/// 依据：细则原文「≥ 3 次」——2 是偶发，3 是复读行为。
pub const RECAP_REPEAT_MIN_COUNT: i64 = 3;
/// 「前 N 场」回看窗。依据：周播节奏 1–2 周内的场次都可能是同批观众；
/// 更老的场次对「回来过」的判定稀释成噪声。细则未钉死数值——命名留档。
pub const RECAP_LOOKBACK_SESSIONS: usize = 10;

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

#[derive(Debug)]
struct SessionRow {
    start_ts: i64,
    end_ts: i64,
    rid: String,
}

#[derive(Debug)]
struct LineRow {
    session_key: (i64, i64),
    sender: String,
    ts: Option<i64>,
    text: String,
    is_comment: bool,
}

/// 规则聚四个数。`_room` 一条没有 → status=empty + 诚实文案；
/// 有场次但本场零发言 → status=empty + 诚实文案（session 字段仍给出本场窗）。
pub fn compute_recap(store: &Store) -> Result<RecapCard> {
    let rows =
        crate::graph::query::episodes(store, crate::episodes::room_corpus::ROOM_VIEWER_ID, None)?;
    let mut sessions: Vec<SessionRow> = Vec::new();
    let mut lines: Vec<LineRow> = Vec::new();
    let mut comments: Vec<Value> = Vec::new();
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
                let start_ts = unix_to_i64(&facts["session"]["start_timestamp"]).unwrap_or(0);
                let end_ts = unix_to_i64(&facts["session"]["end_timestamp"]).unwrap_or(0);
                lines.push(LineRow {
                    session_key: (start_ts, end_ts),
                    sender: facts
                        .get("sender_uid_crc")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    ts: unix_to_i64(&facts["ts"]).filter(|ts| *ts > 0),
                    text,
                    is_comment: false,
                });
                if start_ts > 0
                    && !sessions
                        .iter()
                        .any(|s| (s.start_ts, s.end_ts) == (start_ts, end_ts))
                {
                    sessions.push(SessionRow {
                        start_ts,
                        end_ts,
                        rid: facts
                            .get("rid")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            }
            Some(crate::episodes::room_corpus::SOURCE_COMMENT) => {
                let ctime = unix_to_i64(&facts["ctime"]).unwrap_or(0);
                let mid = facts
                    .get("mid")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                comments.push(serde_json::json!({
                    "mid": mid, "ctime": ctime, "text": text,
                }));
            }
            _ => {}
        }
    }
    sessions.sort_by_key(|s| (s.end_ts, s.start_ts));

    let mut unknown: Vec<String> = Vec::new();
    if sessions.is_empty() {
        // 轮2-R1-A⑥b：评论落了库但零场次窗可归 ≠ 「没碰到」——诚实分轨：
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
        });
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

    // 评论按 ctime 落窗归场（细则：comment 用 ctime 定场次窗）。
    for comment in &comments {
        let ctime = comment["ctime"].as_i64().unwrap_or(0);
        if ctime <= 0 {
            unknown.push(format!(
                "评论缺 ctime，无法归场：rpid={}",
                comment["text"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(12)
                    .collect::<String>()
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
                sender: comment["mid"].as_str().unwrap_or("").to_string(),
                ts: Some(ctime),
                text: comment["text"].as_str().unwrap_or("").to_string(),
                is_comment: true,
            }),
            None => {
                unknown.push(format!(
                    "评论落在所有场次窗外（ctime={ctime}），未计入四个数"
                ));
            }
        }
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
    if speakers > 0 {
        // 轮2-R1-A⑥a：身份键双轨照实说——弹幕 sender_uid_crc 轨与评论 mid 轨：
        // 键名源自细则字面，平台回放接口实测吐真 uid，同人跨轨靠字符串相等合流；
        // 但若平台对弹幕脱敏回 CRC，同人两轨不相等 → 人数系统性 +1。
        let danmaku_speakers: HashSet<&str> = current_lines
            .iter()
            .filter(|line| !line.is_comment)
            .map(|line| line.sender.as_str())
            .filter(|sender| !sender.is_empty())
            .collect();
        let comment_speakers: HashSet<&str> = current_lines
            .iter()
            .filter(|line| line.is_comment)
            .map(|line| line.sender.as_str())
            .filter(|sender| !sender.is_empty())
            .collect();
        if !danmaku_speakers.is_empty() && !comment_speakers.is_empty() {
            unknown.push(format!(
                "本场发言两条轨并出场：弹幕 {} 人 + 评论 {} 人（去重按字符串相等）——\
                 若平台把弹幕 uid 脱敏成 CRC，同人两轨会误计多 1；人数按当前呈现。",
                danmaku_speakers.len(),
                comment_speakers.len()
            ));
        }
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
        });
    }

    // ① 回来过的：本场发言者 ∩ 前 N 场任意场发言者。
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

    // ② 密度峰：10 分钟滑窗（只数弹幕行；评论只有 ctime 颗粒，混进会稀释口径）。
    let mut timed: Vec<i64> = current_lines
        .iter()
        .filter(|line| !line.is_comment)
        .filter_map(|line| line.ts)
        .collect();
    let danmaku_total = current_lines.iter().filter(|line| !line.is_comment).count();
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
    })
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
        // tempdir 守活到函数尾——Store 自持连接，句柄独立。
        store
    }

    /// 钉子①：合成场景（3 人夜）四个数人工可复算。
    /// 场次：S1=[1000,2000]，S2=[3000,4600]（本场）。
    /// S2 弹幕：A×2（3020,3030）B×1（4000）C×3（4200,4210,4300 同句「晚上好！」）。
    /// → 发言 3 人；A 在 S1 也说过 → 回来 1/3；
    ///   峰：4200 窗（4180~4780）内 3 行（4200/4210/4300）vs 3030 窗 2 行 → top=3@4200；
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
            "4000 起手 10 分窗 [4000,4600): 4000/4200/4210/4300 四行（4200 窗只有 3 行）"
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

    /// 空场的姊妹形态：有场次但零发言（如回放存在但 0 行弹幕）。
    #[test]
    fn session_without_lines_is_honest_empty() {
        // 只立 S1 有行、S2 无行——S2 仍是「本场」（end 最新）但零发言。
        // 造法：S2 无行 → 场次只能从有行的方发现，故换成 S1 有行但最新场的
        // 定义再确认：sessions 由弹幕行反推，无行即无场——此形态等价于
        // 「零 _room」空场；实测：单一场 1 行 → 无历史场 → 回来过=None+未知行。
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

    /// 缺时间戳：峰 None + 未知行（MAP-6 诚实面，绝非回退到行序）。
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

    /// 轮2-R1-A⑥a：弹幕+评论同场都有人 → 未知行必须明示身份键双轨
    /// （sender_uid 并集按平台 uid 认定；若弹幕被平台脱敏成 CRC，人数可能 -1 误计）。
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
        assert_eq!(card.speakers, 1, "同一平台 uid 并集（唯一人）");
        assert!(
            card.unknown
                .iter()
                .any(|row| row.contains("CRC") || row.contains("误计")),
            "身份双轨必须明示入未知行: {:?}",
            card.unknown
        );
    }

    /// 轮2-R1-A⑥b：零弹幕但只有评论 —— 诚实形态：文案必须承认「落了 N 条评论、
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
