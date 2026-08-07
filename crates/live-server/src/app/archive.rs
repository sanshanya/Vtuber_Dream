//! 存档页数据面（「里程碑日历 + 周健康」，裁决后接替图谱进主导航）。
//!
//! 本任务是纯事实派生面（零 AI）：全部数字来自既存落盘事实文件——
//! collection.json / streamer.json / ai/recap.json / history 快照 / 图库场次 /
//! history/follower_snapshots.jsonl。哪条缺哪条落 unknown（「未就位」句式），
//! 绝不猜测补数。
//!
//! 时间计算纪律：live-server 源箱不引 chrono（registry.rs 先例「不新拉 chrono
//! 进 live-server」）——秒计数全走 i64 unix ± civil 日历纯整数算法（Hinnant
//! civil），ISO 形态经 live_core::episodes 既有助手输出。

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};
use tokio::task::spawn_blocking;

use crate::app::{AppResult, AppState, data_root, internal, open_graph};

/// 周健康固定两行的未知态文案（对应模块各自缺件时的唯一「未就位」表述）。
const CORE_DANMAKU_UNKNOWN: &str = "名册分档未就位（D2 冻结）";
const FOLLOWER_DELTA_UNKNOWN: &str = "快照未就位（刚建账）";
const GUARD_DELTA_UNKNOWN: &str = "上轮舰长数据未就位";
const REPEAT_RATE_UNKNOWN: &str = "复读率未就位";

/// 时间型里程碑三件（天数 → 达成阈）。周年按平年 365 天计（不作闰年校正——
/// 展示口径足够，且首播后的首个周年日在跨闰区最大差 1 天，里程碑语义不受影响）。
const TIME_MILESTONES: [(&str, &str, i64); 3] = [
    ("full_moon", "满月", 30),
    ("hundred_days", "百天", 100),
    ("anniversary", "周年", 365),
];

// ---------------------------------------------------------------------------
// civil 日历纯整数算法（Hinnant，无时区，UTC 面)
// ---------------------------------------------------------------------------

/// 公历 (y, m, d) → 自 1970-01-01 起的天数（Hinnant civil 算法）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 自 1970-01-01 起的天数 → 公历 (y, m, d)。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// ISO 时间串 → unix 秒。只收 UTC 系尾缀（Z/z/缺失/+00:00/+0000）——其他偏移
/// 一律拒绝（宁可落空跳来源，不按猜的时区出日期）。定形前 19 字符
/// YYYY-MM-DDTHH:MM:SS；回首校验滤 2/30 等非法日组合。
fn parse_iso_utc(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |lo: usize, hi: usize| -> Option<i64> {
        std::str::from_utf8(&b[lo..hi]).ok()?.parse::<i64>().ok()
    };
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let h = num(11, 13)?;
    let mi = num(14, 16)?;
    let s = num(17, 19)?;
    let tail = &text[19..];
    // 吃掉可选的 `.ffffff` 小数段后再校验时区尾（now_iso 形态必带 .ffffff+00:00）。
    let after_frac = tail
        .strip_prefix('.')
        .map(|frac| {
            let digits: usize = frac.chars().take_while(|ch| ch.is_ascii_digit()).count();
            &frac[digits..]
        })
        .unwrap_or(tail);
    let utc_ok = after_frac.is_empty()
        || after_frac.starts_with('Z')
        || after_frac.starts_with('z')
        || after_frac.starts_with("+00:00")
        || after_frac.starts_with("+0000");
    if !utc_ok || !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 61 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    let (ry, rm, rd) = civil_from_days(days);
    if (ry, rm, rd) != (y, mo, d) {
        return None;
    }
    Some(days * 86400 + h * 3600 + mi * 60 + s)
}

/// 归档目录名（%Y%m%dT%H%M%SZ）→ unix 秒。archive_current_snapshot 同秒冲突
/// 的后缀（-1/-2）由 15 位定形前缀自然忽略（不参与解析即不参与取锚）。
fn parse_snapshot_stamp(name: &str) -> Option<i64> {
    let b = name.as_bytes();
    if b.len() < 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let num = |lo: usize, hi: usize| -> Option<i64> {
        std::str::from_utf8(&b[lo..hi]).ok()?.parse::<i64>().ok()
    };
    let y = num(0, 4)?;
    let mo = num(4, 6)?;
    let d = num(6, 8)?;
    let h = num(9, 11)?;
    let mi = num(11, 13)?;
    let s = num(13, 15)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 61 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    let (ry, rm, rd) = civil_from_days(days);
    if (ry, rm, rd) != (y, mo, d) {
        return None;
    }
    Some(days * 86400 + h * 3600 + mi * 60 + s)
}

/// unix 秒 → `YYYY-MM-DD`（UTC）。时间型里程碑的达成日显示用。
fn format_utc_date(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(86400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// i64 → 三位分组数字串（「1,000」；负数保留负号）。涨粉 delta 的展示习惯。
fn format_with_thousands(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.char_indices() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if negative {
        out.insert(0, '-');
    }
    out
}

// ---------------------------------------------------------------------------
// 事实读取
// ---------------------------------------------------------------------------

/// 与 rooms.rs 同款：读失败一律按空态处理（事实缺失 = unknown 行，不报 500）。
fn read_json(path: &Path) -> Option<Value> {
    match live_core::storage::read_json(path) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("存档页读取 JSON 失败（按空态处理）：{err}");
            None
        }
    }
}

/// 存活起点候选集合：所有可用来源逐层收集，最终取最小（最早）。
/// (a) 当前 collection.json.started_at + 每个历史快照目录内的 collection.json.started_at；
/// (b) 快照目录名时间戳（%Y%m%dT%H%M%SZ，同秒冲突后缀自然忽略）；
/// (c) 图库场次最早 observed_at（viewer_id="" 全量读，行多时只保留极值）；
/// (d) shared/live_records.json 记录最早 start_time（unix 秒直接收）。
/// history/follower_snapshots.jsonl 的 ts 不入列：它记录的是粉丝数拐点而非
/// 直播活动起点，作为「存活」锚缺乏语义（口径注在页头）。
fn collect_anchor_candidates(root: &Path) -> Vec<i64> {
    let mut candidates: Vec<i64> = Vec::new();
    if let Some(secs) = read_json(&root.join("collection.json")).and_then(|v| {
        v.get("started_at")
            .and_then(Value::as_str)
            .and_then(parse_iso_utc)
    }) {
        candidates.push(secs);
    }
    let snapshots_dir = root.join("history").join("snapshots");
    if let Ok(entries) = std::fs::read_dir(&snapshots_dir) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str().map(String::from) {
                if let Some(secs) = parse_snapshot_stamp(&name) {
                    candidates.push(secs);
                }
                if let Some(secs) = read_json(&entry.path().join("collection.json")).and_then(|v| {
                    v.get("started_at")
                        .and_then(Value::as_str)
                        .and_then(parse_iso_utc)
                }) {
                    candidates.push(secs);
                }
            }
        }
    }
    if let Some(store) = open_graph(root)
        && let Ok(rows) = live_core::graph::query::episodes(&store, "", None)
    {
        // 行序 observed_at DESC——倒序到最早只需保住首条可达的最早值。
        for row in rows.iter().rev() {
            if let Some(secs) = row
                .get("observed_at")
                .and_then(Value::as_str)
                .and_then(parse_iso_utc)
            {
                candidates.push(secs);
                break;
            }
        }
    }
    if let Some(value) = read_json(&root.join("shared").join("live_records.json"))
        && let Some(rows) = value.get("records").and_then(Value::as_array)
    {
        for row in rows {
            if let Some(secs) = row
                .get("start_time")
                .and_then(live_core::episodes::value_unix_secs)
                && secs > 0
            {
                candidates.push(secs);
            }
        }
    }
    candidates
}

/// 最近一档快照（目录名时间戳最大者）内 collection.json 的舰长数（上一轮值）。
fn latest_snapshot_viewer_count(root: &Path) -> Option<i64> {
    let snapshots_dir = root.join("history").join("snapshots");
    let mut latest: Option<(i64, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(&snapshots_dir) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            let Some(secs) = parse_snapshot_stamp(&name) else {
                continue;
            };
            if latest.as_ref().is_none_or(|(top, _)| secs > *top) {
                latest = Some((secs, entry.path()));
            }
        }
    }
    let (_, dir) = latest?;
    read_json(&dir.join("collection.json"))?
        .get("viewer_count")?
        .as_i64()
}

/// history/follower_snapshots.jsonl → 首尾有序的 followers 序列（坏行/缺粉丝字段跳过；
/// 纪律：文件跟随 collect 逐轮写、等值不追加、缺位不记——见落账口径）。
fn read_follower_snapshots(root: &Path) -> Vec<i64> {
    let path = root.join("history").join("follower_snapshots.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(followers) = value.get("followers").and_then(Value::as_i64) {
            out.push(followers);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 周健康四数
// ---------------------------------------------------------------------------

/// 复读率 = recap.json repeated.count / speakers（同一场次内的配比口径）。
/// 缺 recap.json / repeated / speakers 一律 unknown「复读率未就位」。
fn repeat_rate_row(root: &Path) -> Value {
    let recap = read_json(&root.join("ai").join("recap.json"));
    let count = recap
        .as_ref()
        .and_then(|v| v.get("repeated"))
        .and_then(|v| v.get("count"))
        .and_then(Value::as_i64);
    let speakers = recap
        .as_ref()
        .and_then(|v| v.get("speakers"))
        .and_then(Value::as_i64);
    if let (Some(count), Some(speakers)) = (count, speakers)
        && count >= 0
        && speakers > 0
    {
        let percent = (count as f64 / speakers as f64 * 100.0).round() as i64;
        return json!({
            "key": "repeat_rate",
            "label": "复读率",
            "value_text": format!("{percent}%（{count} 次 / {speakers} 人）"),
            "known": true,
        });
    }
    json!({
        "key": "repeat_rate",
        "label": "复读率",
        "value_text": REPEAT_RATE_UNKNOWN,
        "known": false,
    })
}

/// 大航海 delta = 当前 collection.viewer_count − 最近一档快照（上一轮）的
/// collection.viewer_count。缺任意一侧 → unknown「上轮舰长数据未就位」。
fn guard_delta_row(root: &Path) -> Value {
    let current = read_json(&root.join("collection.json"))
        .and_then(|v| v.get("viewer_count").and_then(Value::as_i64));
    let prev = latest_snapshot_viewer_count(root);
    if let (Some(current), Some(prev)) = (current, prev) {
        let delta = current - prev;
        return json!({
            "key": "guard_delta",
            "label": "大航海 delta",
            "value_text": format!("{delta:+}（上轮 {prev} → 现轮 {current}）"),
            "known": true,
        });
    }
    json!({
        "key": "guard_delta",
        "label": "大航海 delta",
        "value_text": GUARD_DELTA_UNKNOWN,
        "known": false,
    })
}

/// 涨粉 delta（实数化）：末行 − 从末往前第一条异值行；全同值 → delta=0 且
/// 注明「自建账起未变」。行数 <2 → unknown「快照未就位（刚建账）」。
fn follower_delta_row(root: &Path) -> Value {
    let followers = read_follower_snapshots(root);
    if let (Some(last), first_different) = (
        followers.last(),
        followers
            .iter()
            .rev()
            .skip(1)
            .find(|value| **value != *followers.last().unwrap()),
    ) {
        // 全同值时 first_different 为 None → prev = last → delta = 0。
        let prev = first_different.copied().unwrap_or(*last);
        let delta = last - prev;
        // 手工带号：正数才打「+」（「+0」形态对「自建账起未变」是误导）。
        let signed = if delta > 0 {
            format!("+{delta}")
        } else {
            delta.to_string()
        };
        let pair = if prev == *last {
            format!("{}，自建账起未变", format_with_thousands(*last))
        } else {
            format!(
                "{} → {}",
                format_with_thousands(prev),
                format_with_thousands(*last)
            )
        };
        return json!({
            "key": "follower_delta",
            "label": "涨粉 delta",
            "value_text": format!("{signed}（{pair}）"),
            "known": true,
        });
    }
    json!({
        "key": "follower_delta",
        "label": "涨粉 delta",
        "value_text": FOLLOWER_DELTA_UNKNOWN,
        "known": false,
    })
}

// ---------------------------------------------------------------------------
// 里程碑日历
// ---------------------------------------------------------------------------

/// 时间型里程碑（满月/百天/周年）：锚点缺失 → unknown「起始锚点未就位」；
/// 存活天数达到阈 → done（达成日 = 锚点 + N 天）；否则 pending（还差 N−cur 天）。
fn time_milestone_row(
    key: &str,
    label: &str,
    days: i64,
    earliest: Option<i64>,
    alive_days: Option<i64>,
) -> Value {
    match (earliest, alive_days) {
        (Some(since), Some(current)) if current >= days => json!({
            "key": key,
            "label": label,
            "state": "done",
            "detail_text": format!(
                "已于 {} 达成（存活 {current} 天）",
                format_utc_date(since + days * 86400)
            ),
        }),
        (_, Some(current)) => json!({
            "key": key,
            "label": label,
            "state": "pending",
            "detail_text": format!("还差 {} 天（存活 {current} 天）", days - current),
        }),
        _ => json!({
            "key": key,
            "label": label,
            "state": "unknown",
            "detail_text": "起始锚点未就位",
        }),
    }
}

/// 千粉里程碑：streamer.json profile.followers ≥ 1000 为达成。
fn follower_milestone_row(root: &Path) -> Value {
    let followers = read_json(&root.join("streamer.json"))
        .and_then(|v| v.pointer("/profile/followers").and_then(Value::as_i64));
    match followers {
        Some(followers) if followers >= 1000 => json!({
            "key": "thousand_followers",
            "label": "千粉",
            "state": "done",
            "detail_text": format!("粉丝 {followers}（目标 1000）达成"),
        }),
        Some(followers) => json!({
            "key": "thousand_followers",
            "label": "千粉",
            "state": "pending",
            "detail_text": format!("还差 {} 粉（当前 {followers}）", 1000 - followers),
        }),
        None => json!({
            "key": "thousand_followers",
            "label": "千粉",
            "state": "unknown",
            "detail_text": "粉丝数未就位",
        }),
    }
}

/// 百舰里程碑：当前 collection.json viewer_count（舰长数语义）≥ 100 为达成。
fn guard_milestone_row(root: &Path) -> Value {
    let viewer_count = read_json(&root.join("collection.json"))
        .and_then(|v| v.get("viewer_count").and_then(Value::as_i64));
    match viewer_count {
        Some(viewer_count) if viewer_count >= 100 => json!({
            "key": "hundred_guards",
            "label": "百舰",
            "state": "done",
            "detail_text": format!("舰长 {viewer_count}（目标 100）达成"),
        }),
        Some(viewer_count) => json!({
            "key": "hundred_guards",
            "label": "百舰",
            "state": "pending",
            "detail_text": format!("还差 {} 舰（当前 {viewer_count}）", 100 - viewer_count),
        }),
        None => json!({
            "key": "hundred_guards",
            "label": "百舰",
            "state": "unknown",
            "detail_text": "舰长数未就位",
        }),
    }
}

// ---------------------------------------------------------------------------
// 主入口 + HTTP 端点
// ---------------------------------------------------------------------------

/// 存档页数据面主入口（纯函数，now 注入以便确定性测试；HTTP 处理器传当前时钟）。
/// 响应形（规格钉）：见 tests/app_archive.rs。
pub fn build_archive_payload(root: &Path, now_unix: i64) -> Value {
    let earliest = collect_anchor_candidates(root).into_iter().min();
    // 口径：锚点必须 > 0（时间戳非法的奇怪值只可能是坏数据，宁缺）。
    let anchor = earliest.filter(|unix| *unix > 0);
    let alive_since: Option<String> = anchor
        .map(|unix| live_core::episodes::unix_secs_to_iso(Some(unix)))
        .filter(|iso| !iso.is_empty());
    let alive_days: Option<i64> = anchor.map(|unix| {
        let span = now_unix.saturating_sub(unix).max(0);
        span / 86400
    });

    let weekly_health = json!([
        repeat_rate_row(root),
        json!({
            "key": "core_danmaku_group",
            "label": "核心弹幕团",
            "value_text": CORE_DANMAKU_UNKNOWN,
            "known": false,
        }),
        guard_delta_row(root),
        follower_delta_row(root),
    ]);

    // 键序 = 规格文序（满月/百天/周年/千粉/百舰）——前端按数组序直渲（app_archive.rs 钉）。
    let milestones = json!([
        time_milestone_row(
            "full_moon",
            "满月",
            TIME_MILESTONES[0].2,
            anchor,
            alive_days
        ),
        time_milestone_row(
            "hundred_days",
            "百天",
            TIME_MILESTONES[1].2,
            anchor,
            alive_days,
        ),
        time_milestone_row(
            "anniversary",
            "周年",
            TIME_MILESTONES[2].2,
            anchor,
            alive_days
        ),
        follower_milestone_row(root),
        guard_milestone_row(root),
    ]);

    json!({
        "alive_days": alive_days,
        "alive_since": alive_since,
        "weekly_health": weekly_health,
        "milestones": milestones,
    })
}

/// GET /api/archive —— 存档页数据面（存活天数 + 周健康 + 里程碑日历）。
pub(super) async fn archive_get(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let root = data_root(&state)?;
    let payload =
        spawn_blocking(move || build_archive_payload(&root, live_core::episodes::now_unix_secs()))
            .await
            .map_err(internal)?;
    Ok(Json(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-06-01 / 2026-08-06 的固定 unix 常数（与 tests/app_archive.rs 同源）。
    const JUN1_2026: i64 = 1_780_272_000;
    const AUG6_2026: i64 = 1_785_974_400;

    #[test]
    fn civil_roundtrip_reference_dates() {
        assert_eq!(
            days_from_civil(2026, 8, 6) - days_from_civil(2026, 6, 1),
            66
        );
        for (y, m, d) in [
            (2026, 6, 1),
            (2026, 8, 6),
            (2000, 2, 29),
            (2024, 12, 31),
            (1970, 1, 1),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(
            parse_iso_utc("2026-06-01T00:00:00.000000+00:00"),
            Some(JUN1_2026)
        );
        assert_eq!(parse_iso_utc("2026-08-06T00:00:00+00:00"), Some(AUG6_2026));
    }

    #[test]
    fn iso_parse_rejects_non_utc_and_garbage() {
        assert_eq!(
            parse_iso_utc("2026-01-01T00:00:00+08:00"),
            None,
            "非 UTC 偏移拒绝"
        );
        assert_eq!(parse_iso_utc("2025-02-30T00:00:00Z"), None, "非法日期拒绝");
        assert_eq!(parse_iso_utc("not-a-date"), None);
        assert_eq!(parse_snapshot_stamp("20260601T000000Z"), Some(JUN1_2026));
        assert_eq!(
            parse_snapshot_stamp("20260601T000000Z-1"),
            Some(JUN1_2026),
            "同秒冲突后缀忽略"
        );
        assert_eq!(parse_snapshot_stamp("20260601T000000"), None, "缺 Z 尾拒绝");
    }

    #[test]
    fn thousands_format() {
        assert_eq!(format_with_thousands(0), "0");
        assert_eq!(format_with_thousands(1000), "1,000");
        assert_eq!(format_with_thousands(1_002), "1,002");
        assert_eq!(format_with_thousands(-1002), "-1,002");
        assert_eq!(format_with_thousands(12_345_678), "12,345,678");
    }
}
