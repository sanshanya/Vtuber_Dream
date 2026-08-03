//! M4.x 薄切线索环：JSONL 账本（design §16「M4.x」；私仓 kickoff 2026-08-05）。
//!
//! - 账本 = `output_dir/leads.jsonl`，每行 JSON 一个 lead；身份 = (type, locator)
//!   的 `dedupe_key`（hash_parts 同源 sha1·16hex），同键任意状态再写入 → 跳行（幂等）。
//! - 状态机：pending_approval →（人工改行）approved →（collect 尾段按预算消费）
//!   consumed + yield_count；人工可改 rejected；适配器无映射的类型写 deferred。
//!   禁倒退路径。
//! - fail-open（kickoff D7）：账本失败 = 丢账目不丢感知——`read_*` 对不存在/
//!   不可读文件返回空集、坏行静默跳；`record_*` 的 Err 由调用方 `let _ =` 吞。
//! - 摘要段是下轮 AI 上下文唯一消费者（移除实验体：不在则死）。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::episodes::hash_parts;
use crate::models::Lead;

/// 账本文件名（kickoff D1：现布局单房间产物根，`rooms/{uid}` 在 M5 多房间时映射）。
pub const LEDGER_FILE_NAME: &str = "leads.jsonl";
/// `dedupe_key` recipe 域前缀（`_hash` 第一槽）。
pub const DEDUPE_KEY_PREFIX: &str = "m4x-lead";
/// 摘要段 latest_consumed 最多回放的消费行数（kickoff 契约）。
pub const LATEST_CONSUMED_CAP: usize = 3;
/// audience 侧账本行 viewer_id 占位（leads 来自整体态势终局提交）。
pub const AUDIENCE_VIEWER_ID: &str = "audience";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadStatus {
    PendingApproval,
    Approved,
    Consumed,
    Rejected,
    Deferred,
}

/// 账本行（kickoff 契约冻结列；人工可读可编辑，字段序即书写序）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerRow {
    pub dedupe_key: String,
    #[serde(rename = "type")]
    pub lead_type: String,
    pub locator: String,
    pub motivation: String,
    pub expected_signal: String,
    pub priority: String,
    pub evidence_ids: Vec<String>,
    pub viewer_id: String,
    pub first_seen_run_id: String,
    pub created_at: String,
    pub status: LeadStatus,
    pub yield_count: i64,
    #[serde(default)]
    pub resolution_note: String,
}

/// `_hash(f"m4x-lead|{type}|{locator}", 16)`：可操作目标即身份，motivation 措辞不入。
pub fn dedupe_key(lead: &Lead) -> String {
    hash_parts(
        &[
            DEDUPE_KEY_PREFIX.to_string(),
            lead.lead_type.clone(),
            lead.locator.clone(),
        ],
        16,
    )
}

/// 新 lead → pending 行（状态机入口的唯一构造点）。
fn pending_row(lead: &Lead, viewer_id: &str, run_id: &str, now: &str) -> LedgerRow {
    LedgerRow {
        dedupe_key: dedupe_key(lead),
        lead_type: lead.lead_type.clone(),
        locator: lead.locator.clone(),
        motivation: lead.motivation.clone(),
        expected_signal: lead.expected_signal.clone(),
        priority: lead.priority.clone(),
        evidence_ids: lead.evidence_ids.clone(),
        viewer_id: viewer_id.to_string(),
        first_seen_run_id: run_id.to_string(),
        created_at: now.to_string(),
        status: LeadStatus::PendingApproval,
        yield_count: 0,
        resolution_note: String::new(),
    }
}

pub fn ledger_path(output_dir: &Path) -> PathBuf {
    output_dir.join(LEDGER_FILE_NAME)
}

/// fail-open 读：不存在/不可读 → 空集；坏行（非 JSON / 缺键）静默跳过。
/// 只喂**读面**（annex/摘要）；写面必须用 `read_ledger_guarded`（MXA-1 防线）。
pub fn read_ledger(path: &Path) -> Vec<LedgerRow> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<LedgerRow>(trimmed).ok()
        })
        .collect()
}

/// 守卫读（MXA-1）：不存在 → 空集；存在但任何一行不可解析 → Err。
/// 防「fail-open 读损集合被写面误当全景 → 同键重复追加」（验收「重跑不增行」防线）。
pub fn read_ledger_guarded(path: &Path) -> std::io::Result<Vec<LedgerRow>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row = serde_json::from_str::<LedgerRow>(trimmed).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("leads.jsonl 第{}行不可解析：{err}", index + 1),
            )
        })?;
        rows.push(row);
    }
    Ok(rows)
}

/// 追加新线索（幂等）：同 dedupe_key 任意状态已存在则跳行。返回实际追加行数。
pub fn record_leads(
    output_dir: &Path,
    viewer_id: &str,
    run_id: &str,
    now: &str,
    leads: &[Lead],
) -> std::io::Result<usize> {
    let path = ledger_path(output_dir);
    // MXA-1：写面用守卫读——读损即 Err（调用方响铃），绝不带病重复追加。
    let existing: std::collections::HashSet<String> = read_ledger_guarded(&path)?
        .into_iter()
        .map(|row| row.dedupe_key)
        .collect();
    let fresh: Vec<LedgerRow> = leads
        .iter()
        .map(|lead| pending_row(lead, viewer_id, run_id, now))
        .filter(|row| !existing.contains(&row.dedupe_key))
        .collect();
    if fresh.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(output_dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for row in &fresh {
        let line = serde_json::to_string(row).expect("账本行可序列化");
        writeln!(file, "{line}")?;
    }
    Ok(fresh.len())
}

/// 整账本重写（消费写回用；tmp+rename 与 storage 原子替换同款纪律）。
pub fn rewrite_ledger(output_dir: &Path, rows: &[LedgerRow]) -> std::io::Result<()> {
    let path = ledger_path(output_dir);
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        for row in rows {
            let line = serde_json::to_string(row).expect("账本行可序列化");
            writeln!(file, "{line}")?;
        }
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// kickoff 契约摘要段。`viewer=None` → 全局行；`Some(v)` → 前缀 `viewer=… own_pending=…`。
/// by_type 只计未被 reject 的行（死账不喂回下轮），类型按名字排序序保证确定性。
pub fn summary_line(rows: &[LedgerRow], viewer: Option<&str>) -> String {
    let count = |status: LeadStatus| rows.iter().filter(|r| r.status == status).count();
    let mut by_type: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for row in rows.iter().filter(|r| r.status != LeadStatus::Rejected) {
        *by_type.entry(row.lead_type.as_str()).or_insert(0) += 1;
    }
    let by_type = by_type
        .iter()
        .map(|(name, n)| format!("{name}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let yield_total: i64 = rows.iter().map(|r| r.yield_count).sum();
    let latest: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| r.status == LeadStatus::Consumed)
        .rev()
        .take(LATEST_CONSUMED_CAP)
        .map(|r| {
            serde_json::json!({
                "type": r.lead_type,
                "locator": r.locator,
                "yield_count": r.yield_count,
            })
        })
        .collect();
    let head = match viewer {
        Some(v) => {
            let own_pending = rows
                .iter()
                .filter(|r| r.viewer_id == v && r.status == LeadStatus::PendingApproval)
                .count();
            format!("[lead_ledger] viewer={v} own_pending={own_pending} |")
        }
        None => "[lead_ledger]".to_string(),
    };
    format!(
        "{head} pending={} approved={} consumed={} rejected={} deferred={} \
         by_type={{{by_type}}} yield_total={yield_total} latest_consumed={latest}",
        count(LeadStatus::PendingApproval),
        count(LeadStatus::Approved),
        count(LeadStatus::Consumed),
        count(LeadStatus::Rejected),
        count(LeadStatus::Deferred),
        latest = serde_json::to_string(&latest).expect("latest 可序列化"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead(lead_type: &str, locator: &str) -> Lead {
        Lead {
            lead_type: lead_type.to_string(),
            locator: locator.to_string(),
            motivation: "m".to_string(),
            expected_signal: "s".to_string(),
            priority: "high".to_string(),
            evidence_ids: vec!["e1".to_string()],
        }
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("m4x-leads-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 契约：dedupe_key = hash_parts(["m4x-lead", type, locator], 16)。
    /// 自预言机纪律：用手工拼出的 parts 对照，钉 join 序与域前缀。
    #[test]
    fn dedupe_key_recipe_pinned() {
        let row = lead("video", "BV1aa411c7mD");
        assert_eq!(
            dedupe_key(&row),
            hash_parts(
                &[
                    "m4x-lead".to_string(),
                    "video".to_string(),
                    "BV1aa411c7mD".to_string()
                ],
                16
            )
        );
        // (type, locator) 是身份：motivation/evidence 不入键
        let mut same = row.clone();
        same.motivation = "另一措辞".to_string();
        same.evidence_ids = vec![];
        assert_eq!(dedupe_key(&row), dedupe_key(&same));
        // type 或 locator 变 → 身份变
        let other = lead("creator", "BV1aa411c7mD");
        assert_ne!(dedupe_key(&row), dedupe_key(&other));
    }

    #[test]
    fn record_then_rerun_appends_zero_rows() {
        let dir = tmp_dir("idem");
        let leads = vec![lead("video", "BV1"), lead("search", "恋恋红莲华")];
        let first = record_leads(&dir, "u", "run:a", "2026-08-05T00:00:00+00:00", &leads).unwrap();
        assert_eq!(first, 2);
        // 同输入重跑（缓存命中路径补写）：行数恒定、追加 0
        let again = record_leads(&dir, "u", "run:b", "2026-08-05T01:00:00+00:00", &leads).unwrap();
        assert_eq!(again, 0);
        let rows = read_ledger(&ledger_path(&dir));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, LeadStatus::PendingApproval);
        assert_eq!(rows[0].first_seen_run_id, "run:a");
        assert_eq!(rows[0].yield_count, 0);
        // 新增不同 locator 才追加
        let plus = record_leads(
            &dir,
            "u",
            "run:c",
            "2026-08-05T02:00:00+00:00",
            &[lead("video", "BV2")],
        )
        .unwrap();
        assert_eq!(plus, 1);
        assert_eq!(read_ledger(&ledger_path(&dir)).len(), 3);
    }

    #[test]
    fn read_ledger_skips_malformed_lines() {
        let dir = tmp_dir("bad");
        let path = ledger_path(&dir);
        let good = pending_row(&lead("video", "BV1"), "u", "run:a", "t");
        let text = format!(
            "{}\n{{\"dedupe_key\": \"x\"}}\nnot json at all\n\n",
            serde_json::to_string(&good).unwrap()
        );
        std::fs::write(&path, text).unwrap();
        let rows = read_ledger(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dedupe_key, good.dedupe_key);
        assert!(read_ledger(&dir.join("不存在.jsonl")).is_empty());
    }

    /// MXA-1（r1-F1 负钉）：账本存在但含不可解析行 → record 拒绝写入（同键不能重复
    /// 追加的风险高于丢一笔新线），账本原文不动。
    #[test]
    fn record_refuses_when_ledger_has_bad_line() {
        let dir = tmp_dir("guarded");
        let good = pending_row(&lead("video", "BV1"), "u", "run:a", "t");
        let text = format!(
            "{}\nnot json at all\n",
            serde_json::to_string(&good).unwrap()
        );
        let path = ledger_path(&dir);
        std::fs::write(&path, &text).unwrap();
        let leads = vec![lead("video", "BV2")];
        let result = record_leads(&dir, "u", "run:b", "t2", &leads);
        assert!(result.is_err(), "读损账本必须拒绝追加，防重复写");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "拒绝时账本文本逐字节不动"
        );
        // 读面（fail-open）不受影响：annex/摘要仍拿得到好的那一行
        assert_eq!(read_ledger(&path).len(), 1);
    }

    /// kickoff D7：写入面失败以 Err 返回（调用方 let _ 吞），绝不 panic。
    #[test]
    fn record_failure_surfaces_err_not_panic() {
        let dir = tmp_dir("fail");
        let blocker = dir.join("是文件");
        std::fs::write(&blocker, "x").unwrap();
        let result = record_leads(&blocker, "u", "run:a", "t", &[lead("video", "BV1")]);
        assert!(result.is_err());
    }

    #[test]
    fn summary_line_matches_frozen_contract() {
        let dir = tmp_dir("sum");
        record_leads(
            &dir,
            "u",
            "run:a",
            "t",
            &[lead("video", "BV1"), lead("video", "BV2")],
        )
        .unwrap();
        record_leads(&dir, "v", "run:a", "t", &[lead("creator", "42")]).unwrap();
        let mut rows = read_ledger(&ledger_path(&dir));
        rows[1].status = LeadStatus::Approved;
        // 人本消费：BV1 → consumed yield 5
        rows[0].status = LeadStatus::Consumed;
        rows[0].yield_count = 5;
        let global = summary_line(&rows, None);
        assert_eq!(
            global,
            "[lead_ledger] pending=1 approved=1 consumed=1 rejected=0 deferred=0 \
             by_type={creator: 1, video: 2} yield_total=5 latest_consumed=[{\"type\":\"video\",\"locator\":\"BV1\",\"yield_count\":5}]"
        );
        let scoped = summary_line(&rows, Some("u"));
        assert!(
            scoped.starts_with("[lead_ledger] viewer=u own_pending=0 | pending=1 approved=1"),
            "{scoped}"
        );
        // rows[1]=BV2、rows[2]=creator：rejected(BV2) 不进 by_type；deferred(creator) 计入
        rows[1].status = LeadStatus::Rejected;
        rows[2].status = LeadStatus::Deferred;
        let line = summary_line(&rows, None);
        assert_eq!(
            line,
            "[lead_ledger] pending=0 approved=0 consumed=1 rejected=1 deferred=1 \
             by_type={creator: 1, video: 1} yield_total=5 latest_consumed=[{\"type\":\"video\",\"locator\":\"BV1\",\"yield_count\":5}]"
        );
    }

    #[test]
    fn latest_consumed_capped_at_three() {
        let dir = tmp_dir("cap");
        for i in 0..5 {
            record_leads(&dir, "u", "run:a", "t", &[lead("video", &format!("BV{i}"))]).unwrap();
        }
        let mut rows = read_ledger(&ledger_path(&dir));
        for (i, row) in rows.iter_mut().enumerate() {
            row.status = LeadStatus::Consumed;
            row.yield_count = i as i64;
        }
        let line = summary_line(&rows, None);
        // file 序倒取 3：BV4→BV3→BV2
        assert!(
            line.contains(
                "latest_consumed=[{\"type\":\"video\",\"locator\":\"BV4\",\"yield_count\":4},{\"type\":\"video\",\"locator\":\"BV3\",\"yield_count\":3},{\"type\":\"video\",\"locator\":\"BV2\",\"yield_count\":2}]"
            ),
            "{line}"
        );
        assert!(line.contains("yield_total=10"));
    }
}
