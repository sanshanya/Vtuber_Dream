//! 外部录播 JSONL 回放（离线入图轨）：blivedm 七族底稿 → `WsRecorder` 同轨窗。
//!
//! 底稿契约（录制器是外部件，键名以实测落盘为准——事实只对账不假设）：
//! - `{"t":"danmaku","uid":N,"uname":"…","text":"…","ts":<毫秒>}`
//! - `{"t":"sc","uid":N,"uname":"…","price":F,"text":"…","ts":<秒>`（start_time 原轨）}
//! - `{"t":"gift","uid":N,"uname":"…","gift":"…","num":N,"price":F,"total_coin":N,
//!    "coin_type":"gold|silver","ts":<毫秒>}`
//! - `{"t":"guard","uid":N,"uname":"…","guard_level":N,"ts":<毫秒|null>}`
//! - `{"t":"interact","uid":N,"uname":"…","action":"进入了直播间…","ts":<毫秒|null>}`
//! - `{"t":"popularity","popularity":N,"ts":<毫秒>}`
//! - `{"t":"toast","uid":N(可0),"uname":"…","role":"…","guard_level":N,"ts":<毫秒>}`
//!
//! 纪律（与实时轨同一把尺）：
//! - **身份锚**：uid ≤ 0 / 缺 ts 的行不产 Episode（skipped 计数显形，绝不补数）；
//! - **时戳归一**：底稿各族单位混（毫秒/秒）——`> 1e11 判毫秒除千`的阈值规约只在
//!   本层承担，WsEvent 面一律秒（与协议层 SEND_GIFT 解码同约定）；
//! - **幂等键**：直接复用 WsRecorder 投影（`ws:{room}:{ts_sec}:{uid}:{text_hash16}`，
//!   双机位合并天然撞库去重）；多文件按 ts 稳定排序后喂单窗；
//! - **窗诚实**：起点=首行 ts（attach 标记——回放无 LIVE 校正面对象）；终点=末行
//!   ts；终局原因随 `SessionEnd::Closed` 落 preparing，并显式追加未知行
//!   `UNKNOWN_FILE_TAIL`（文件尾 = 下播/录制终止未区分，不装知道）。
//! - **金额解读**：事实层只记币轨原价（total_coin 金瓜子）；「元」换算
//!   （1000 金瓜子 = 1 元，coin_type=="gold" 才算付费）只进 `ReplayMoney` 摘要——
//!   报表层的解读不动事实层。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use super::episodes::{WsRecorder, WsWindowCapture};
use super::message::WsEvent;
use super::session::SessionEnd;

/// 录播文件尾的未知行文案（诚实面：文件结束的原因本轨无法分辨）。
pub const UNKNOWN_FILE_TAIL: &str = "录播文件尾：下播/录制终止未区分";

/// 毫秒/秒判据：epoch 秒值在 2001-09 后才过 1e9、毫秒值 2001-09 后就过 1e12；
/// 取 1e11 为界（1973 前与 5138 后都错，但直播业务域不到）。
const MS_THRESHOLD: i64 = 100_000_000_000;

/// 分场段距：相邻事件 ts 差超过 2 小时 = 新一场（一晚直播中断 ≤ 秒级——blivedm
/// 断流重连期间以 popularity/重发垫场；真场间隙必越此坪）。一稿多晚 = 多场多窗；
/// 此前单窗压塌三晚 → 复盘「最近场」其实是全合账的语义缺陷（2026-08-10 实账
/// 抓获：25 人/¥35 是全三场，真正本场是 20 人/¥33.8）。
const SESSION_GAP_SECONDS: i64 = 2 * 3600;

/// 金额摘要（报表层）：金瓜子→元的换算只在这里发生（1000:1，gold 才算付费）。
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct ReplayMoney {
    /// 付费礼物行数（coin_type=="gold" 且 total_coin > 0）。
    pub gift_count: u64,
    /// 付费礼物金瓜子总额。
    pub gift_coin_total: i64,
    /// 付费礼物折元（total_coin / 1000）。
    pub gift_yuan: f64,
    /// SC 行数与折元总额（底稿 price 单位即元）。
    pub sc_count: u64,
    pub sc_yuan: f64,
    /// 上舰/播报各自行数（金额面上舰单暂无可结算币轨——行数照登）。
    pub guard_count: u64,
    pub toast_count: u64,
}

/// 回放产出：同轨窗集（每场一窗，段距 ≥ 2h 分场——直接交 `ingest_ws_window`
/// 逐窗入账）+ 行账 + 金额摘要。
#[derive(Debug)]
pub struct ReplayOutcome {
    /// 各场窗（按起点升序；末窗 = 最新场）。
    pub captures: Vec<WsWindowCapture>,
    pub money: ReplayMoney,
    /// 读到的总行数（含被跳行）。
    pub rows: u64,
    /// 被跳行（坏 JSON / 未知 t / 缺身份锚 / 缺时戳）。
    pub skipped: u64,
    /// 各 `t` 值的原文计数（含被跳——出现即登记）。
    pub families: BTreeMap<String, u64>,
}

/// 一个 ts 值 → 秒（`>1e11` 判毫秒除千；≤0 视为缺载 → None）。
fn ts_to_secs(raw: Option<i64>) -> Option<i64> {
    match raw {
        Some(v) if v > MS_THRESHOLD => Some(v / 1000),
        Some(v) if v > 0 => Some(v),
        _ => None,
    }
}

/// JSON 数字 → i64（u64 范围放不下 → None，绝不截断造数）。
fn as_i64(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_i64).or_else(|| {
        v.and_then(Value::as_u64)
            .and_then(|u| i64::try_from(u).ok())
    })
}

/// 单行 JSONL →（时戳秒，事件）。None = 该行本轨不消费（调用侧按原因计数）。
fn row_to_event(row: &Value) -> Option<(i64, WsEvent)> {
    let t = row.get("t").and_then(Value::as_str)?;
    let ts_raw = as_i64(row.get("ts"));
    let uid_raw = as_i64(row.get("uid"));
    // 身份锚纪律在解析层显形：uid 缺载/<=0 的行直接拒（skipped 计数），
    // 零身份事件不往记录器递（避免「喂了但没入账」的影子行）。
    let identity_family = matches!(
        t,
        "danmaku" | "sc" | "gift" | "guard" | "interact" | "toast"
    );
    if identity_family && uid_raw.filter(|u| *u > 0).is_none() {
        return None;
    }
    let uid = || -> String {
        uid_raw
            .filter(|u| *u > 0)
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    let text_of = |key: &str| -> String {
        row.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match t {
        "danmaku" => {
            let ts = ts_to_secs(ts_raw)?;
            Some((
                ts,
                WsEvent::Danmaku {
                    uid: uid(),
                    uname: text_of("uname"),
                    text: text_of("text"),
                    ts: Some(ts),
                },
            ))
        }
        "sc" => {
            let ts = ts_to_secs(ts_raw)?;
            Some((
                ts,
                WsEvent::SuperChat {
                    uid: uid(),
                    uname: text_of("uname"),
                    text: text_of("text"),
                    price: row.get("price").and_then(Value::as_f64).unwrap_or(0.0),
                    start_time: Some(ts),
                },
            ))
        }
        "gift" => {
            let ts = ts_to_secs(ts_raw)?;
            Some((
                ts,
                WsEvent::Gift {
                    uid: uid(),
                    uname: text_of("uname"),
                    gift_name: text_of("gift"),
                    num: as_i64(row.get("num")).unwrap_or(0),
                    price: row.get("price").and_then(Value::as_f64).unwrap_or(0.0),
                    total_coin: as_i64(row.get("total_coin")).unwrap_or(0),
                    coin_type: text_of("coin_type"),
                    ts: Some(ts),
                },
            ))
        }
        "guard" => {
            let ts = ts_to_secs(ts_raw)?;
            Some((
                ts,
                WsEvent::GuardBuy {
                    uid: uid(),
                    uname: text_of("uname"),
                    guard_level: row.get("guard_level").and_then(Value::as_u64).unwrap_or(0) as u32,
                    gift_name: String::new(),
                    num: 1,
                    price: 0.0,
                    ts: Some(ts),
                },
            ))
        }
        "interact" => {
            let ts = ts_to_secs(ts_raw)?;
            // v2 底稿只有动作文案：按协议文档 interp 回 kind
            // （1 进场/2 关注/3 分享）；认不出 → 0 原样登记不明。
            let action = text_of("action");
            let kind = if action.starts_with("进入") {
                1
            } else if action.starts_with("关注") {
                2
            } else if action.starts_with("分享") {
                3
            } else {
                0
            };
            Some((
                ts,
                WsEvent::Interact {
                    kind,
                    uid: uid(),
                    uname: text_of("uname"),
                    ts,
                },
            ))
        }
        "popularity" => {
            let ts = ts_to_secs(ts_raw)?;
            let value = row.get("popularity").and_then(Value::as_i64)?;
            Some((
                ts,
                WsEvent::Popularity {
                    value: u32::try_from(value.max(0)).unwrap_or(0),
                },
            ))
        }
        "toast" => {
            let ts = ts_to_secs(ts_raw)?;
            Some((
                ts,
                WsEvent::Toast {
                    uid: uid(),
                    uname: text_of("uname"),
                    role_name: text_of("role"),
                    guard_level: row.get("guard_level").and_then(Value::as_u64).unwrap_or(0) as u32,
                    ts: Some(ts),
                },
            ))
        }
        _ => None,
    }
}

/// 金额摘要折账（只统计消费事件；事实层币轨原值在 Episode facts 里不动）。
fn fold_money(money: &mut ReplayMoney, ev: &WsEvent) {
    match ev {
        WsEvent::Gift {
            total_coin,
            coin_type,
            ..
        } => {
            if coin_type == "gold" && *total_coin > 0 {
                money.gift_count += 1;
                money.gift_coin_total += total_coin;
                money.gift_yuan += (*total_coin as f64) / 1000.0;
            }
        }
        WsEvent::SuperChat { price, .. } => {
            if *price > 0.0 {
                money.sc_count += 1;
                money.sc_yuan += price;
            }
        }
        WsEvent::GuardBuy { .. } => money.guard_count += 1,
        WsEvent::Toast { .. } => money.toast_count += 1,
        _ => {}
    }
}

/// 读入一/多份录播 JSONL，按 ts 稳定排序后回放进单窗。
///
/// 错误语义：文件打不开 → Err；逐行坏 JSON / 缺锚 → skipped 计数（不致命）。
/// 全文件零可行 → Err（没有可入账的东西，调用侧当失败而非空成功）。
pub fn replay_jsonl(room_id: i64, paths: &[PathBuf]) -> Result<ReplayOutcome, String> {
    if paths.is_empty() {
        return Err("ws-replay 缺文件路径".to_string());
    }
    let mut rows_total: u64 = 0;
    let mut skipped: u64 = 0;
    let mut families: BTreeMap<String, u64> = BTreeMap::new();
    let mut events: Vec<(i64, WsEvent)> = Vec::new();
    for path in paths {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("读录播文件失败（{}）：{e}", path.display()))?;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            rows_total += 1;
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                skipped += 1;
                continue;
            };
            let family = row
                .get("t")
                .and_then(Value::as_str)
                .unwrap_or("<untagged>")
                .to_string();
            *families.entry(family).or_insert(0) += 1;
            match row_to_event(&row) {
                Some(pair) => events.push(pair),
                None => skipped += 1,
            }
        }
    }
    if events.is_empty() {
        return Err(format!(
            "录播无可回放行（读 {rows_total} 行，全被跳）——不装成功"
        ));
    }
    // 稳定排序：同 ts 保文件序（先到的行先落——双机位合并行间相对序无关幂等键）。
    events.sort_by_key(|(ts, _)| *ts);

    // 分场：相邻事件 gap > SESSION_GAP_SECONDS 即新窗（一晚直播不会出 2h 空场；
    // 出 = 采录中断——那也本应分窗各自诚实收尾）。
    let mut segments: Vec<Vec<(i64, WsEvent)>> = Vec::new();
    let mut prev_ts: Option<i64> = None;
    for pair in events {
        match prev_ts {
            Some(prev) if pair.0 - prev > SESSION_GAP_SECONDS => {
                segments.last_mut().expect("段推空前必有旧段");
                segments.push(Vec::new());
            }
            _ => {}
        }
        prev_ts = Some(pair.0);
        match segments.last_mut() {
            Some(seg) => seg.push(pair),
            None => segments.push(vec![pair]),
        }
    }

    let mut money = ReplayMoney::default();
    let mut captures: Vec<WsWindowCapture> = Vec::with_capacity(segments.len());
    for segment in segments {
        let first_ts = segment.first().map(|(ts, _)| *ts).unwrap_or(0);
        let last_ts = segment.last().map(|(ts, _)| *ts).unwrap_or(0);
        let mut recorder = WsRecorder::attach(room_id, first_ts, 1)
            .ok_or_else(|| "回放开窗失败（在播校验常量 1 拒）".to_string())?;
        for (ts, ev) in &segment {
            fold_money(&mut money, ev);
            // recv_ts 喂事件自身的 ts（回放面「收到时刻」= 行时刻，同标不造数）。
            recorder.on_event(ev, *ts);
        }
        let mut capture = recorder.finish(&SessionEnd::Closed, last_ts);
        capture.unknowns.push(UNKNOWN_FILE_TAIL.to_string());
        captures.push(capture);
    }
    Ok(ReplayOutcome {
        captures,
        money,
        rows: rows_total,
        skipped,
        families,
    })
}

#[cfg(test)]
mod tests {
    use super::super::episodes::{SOURCE_WS_DANMAKU, SOURCE_WS_GIFT, SOURCE_WS_TOAST};
    use super::*;

    fn write_tmp(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).expect("写 fixture");
        path
    }

    const FIXTURE: &str = concat!(
        r#"{"t":"popularity","popularity":42,"ts":1700000000000}"#,
        "\n",
        r#"{"t":"danmaku","uid":101,"uname":"甲","text":"好耶","ts":1700000001234}"#,
        "\n",
        r#"{"t":"sc","uid":102,"uname":"金主","price":30.0,"text":"大气","ts":1700000010}"#,
        "\n",
        r#"{"t":"gift","uid":103,"uname":"乙","gift":"小花花","num":2,"price":100.0,"total_coin":200,"coin_type":"gold","ts":1700000020000}"#,
        "\n",
        r#"{"t":"gift","uid":104,"uname":"丙","gift":"辣条","num":1,"price":0.0,"total_coin":0,"coin_type":"silver","ts":1700000030000}"#,
        "\n",
        r#"{"t":"guard","uid":105,"uname":"丁","guard_level":3,"ts":1700000040000}"#,
        "\n",
        r#"{"t":"interact","uid":106,"uname":"戊","action":"进入了直播间","ts":1700000050000}"#,
        "\n",
        r#"{"t":"toast","uid":0,"uname":"","role":"舰长","guard_level":3,"ts":1700000060000}"#,
        "\n",
        r#"{"t":"toast","uid":107,"uname":"己","role":"提督","guard_level":2,"ts":null}"#,
        "\n",
        "这不是 JSON\n",
        r#"{"t":"mystery","uid":1,"ts":1700000070000}"#,
        "\n",
    );

    #[test]
    fn replay_seven_families_fold_to_capture_with_honest_tally() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_tmp(&dir, "a.jsonl", FIXTURE);
        let out = replay_jsonl(9222, std::slice::from_ref(&path)).expect("回放");

        // 行账：11 行（空行不计），toast uid=0 跳、toast 缺 ts 跳、坏 JSON 跳、
        // 未知 t 跳 → skipped=4，进窗 7。
        assert_eq!(out.rows, 11);
        assert_eq!(out.skipped, 4);
        assert_eq!(out.families.get("danmaku"), Some(&1));
        assert_eq!(out.families.get("popularity"), Some(&1));
        assert_eq!(out.families.get("<untagged>"), None);

        assert_eq!(out.captures.len(), 1, "同窗底稿必归单窗");
        let cap = &out.captures[0];
        // 窗：起点=首行（popularity），终点=最大可行 ts。
        assert_eq!(
            cap.session["start_timestamp"],
            serde_json::json!(1_700_000_000)
        );
        assert_eq!(
            cap.session["end_timestamp"],
            serde_json::json!(1_700_000_050)
        );
        assert!(
            cap.unknowns.iter().any(|u| u.contains("录播文件尾")),
            "文件尾未知行必须显形: {:?}",
            cap.unknowns
        );

        // 五线 Episode：弹幕/SC/两礼物/上舰/进场（toast 两线皆跳）。
        assert_eq!(cap.episodes.len(), 6);
        let gift = cap
            .episodes
            .iter()
            .find(|e| e.source == SOURCE_WS_GIFT)
            .expect("礼物线");
        assert_eq!(
            gift.platform_facts["gift_name"],
            serde_json::json!("小花花")
        );
        assert_eq!(gift.platform_facts["total_coin"], serde_json::json!(200));
        assert_eq!(gift.platform_facts["coin_type"], serde_json::json!("gold"));
        assert_eq!(gift.platform_facts["ts"], serde_json::json!(1_700_000_020));
        let dan = cap
            .episodes
            .iter()
            .find(|e| e.source == SOURCE_WS_DANMAKU)
            .expect("弹幕线");
        assert_eq!(
            dan.platform_facts["ts"],
            serde_json::json!(1_700_000_001),
            "毫秒底稿归一秒"
        );
        assert!(
            !cap.episodes.iter().any(|e| e.source == SOURCE_WS_TOAST),
            "uid=0 播报无身份锚不得落 Episode"
        );

        // 金额：gold 礼物 200 金瓜子=0.2 元；silver 不算付费；SC 30 元。
        assert_eq!(out.money.gift_count, 1);
        assert_eq!(out.money.gift_yuan, 0.2);
        assert_eq!(out.money.sc_count, 1);
        assert_eq!(out.money.sc_yuan, 30.0);
        assert_eq!(out.money.guard_count, 1);
        assert_eq!(out.money.toast_count, 0, "两 toast 皆被跳不算行数");

        // 人气只进 counts（最新值）。
        assert_eq!(cap.counts.get("popularity_latest"), Some(&42));
    }

    #[test]
    fn replay_multi_file_merges_by_ts_and_dedups_at_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_tmp(
            &dir,
            "a.jsonl",
            concat!(
                r#"{"t":"danmaku","uid":1,"uname":"甲","text":"早","ts":1700000000000}"#,
                "\n",
                r#"{"t":"danmaku","uid":1,"uname":"甲","text":"晚","ts":1700000009000}"#,
                "\n",
            ),
        );
        // 机位 B：与 A 同一条「早」行（同 ts/uid/text）+ 一条重叠窗中新行。
        let b = write_tmp(
            &dir,
            "b.jsonl",
            concat!(
                r#"{"t":"danmaku","uid":1,"uname":"甲","text":"早","ts":1700000000000}"#,
                "\n",
                r#"{"t":"danmaku","uid":2,"uname":"乙","text":"中","ts":1700000005000}"#,
                "\n",
            ),
        );
        let out = replay_jsonl(9222, &[a, b]).expect("合并回放");
        // 同平台行在同窗内是两份事件——但它们幂等键相同，撞库折叠发生在
        // ingest_ws_window（touch_episode_by_identity）。此处钉的是合并面：
        // 4 行全黎、窗跨全程，重放 Episodes 同窗两份同键行（图层去重）。
        assert_eq!(out.rows, 4);
        assert_eq!(out.skipped, 0);
        assert_eq!(out.captures.len(), 1, "无缝两稿仍归单窗");
        assert_eq!(out.captures[0].episodes.len(), 4);
        assert_eq!(
            out.captures[0].session["start_timestamp"],
            serde_json::json!(1_700_000_000)
        );
        assert_eq!(
            out.captures[0].session["end_timestamp"],
            serde_json::json!(1_700_000_009)
        );
    }

    #[test]
    fn replay_gap_over_two_hours_splits_into_two_windows() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_tmp(
            &dir,
            "two.jsonl",
            concat!(
                r#"{"t":"danmaku","uid":1,"uname":"甲","text":"早场","ts":1700000000000}"#,
                "
",
                r#"{"t":"danmaku","uid":1,"uname":"甲","text":"早场尾","ts":1700001800000}"#,
                "
",
                r#"{"t":"danmaku","uid":2,"uname":"乙","text":"晚场","ts":1700010000000}"#,
                "
",
            ),
        );
        let out = replay_jsonl(9222, &[path]).expect("分场回放");
        assert_eq!(out.captures.len(), 2, ">2h 段距必分双窗");
        let (first, second) = (&out.captures[0], &out.captures[1]);
        assert_eq!(first.episodes.len(), 2);
        assert_eq!(
            first.session["start_timestamp"],
            serde_json::json!(1_700_000_000)
        );
        assert_eq!(
            first.session["end_timestamp"],
            serde_json::json!(1_700_001_800)
        );
        assert_eq!(second.episodes.len(), 1);
        assert_eq!(
            second.session["start_timestamp"],
            serde_json::json!(1_700_010_000)
        );
        assert_eq!(
            second.session["end_timestamp"],
            serde_json::json!(1_700_010_000)
        );
        // 每窗都独立挂文件尾未知行
        for cap in &out.captures {
            assert!(
                cap.unknowns.iter().any(|u| u.contains("录播文件尾")),
                "每窗各自挂文件尾: {:?}",
                cap.unknowns
            );
        }
    }

    #[test]
    fn replay_empty_or_all_skipped_is_error_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let empty = write_tmp(&dir, "empty.jsonl", "");
        let err = replay_jsonl(9222, &[empty]).unwrap_err();
        assert!(err.contains("无可回放行"), "{err}");
        let junk = write_tmp(&dir, "junk.jsonl", "not-json\n{\"t\":\"x\"}\n");
        assert!(replay_jsonl(9222, &[junk]).is_err());
    }

    #[test]
    fn ts_unit_normalization_threshold() {
        assert_eq!(ts_to_secs(Some(1_700_000_000)), Some(1_700_000_000));
        assert_eq!(ts_to_secs(Some(1_700_000_000_000)), Some(1_700_000_000));
        assert_eq!(ts_to_secs(Some(0)), None);
        assert_eq!(ts_to_secs(None), None);
    }
}
