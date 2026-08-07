//! 线索账本（design §8.4 + §9.2 行 254 G2 JSONL→表迁移）。
//!
//! 体积备书：超 500 线 = 账本状态机 + JSONL→表迁移卫 + annex 回喂。
//! annex 段只在「consumed」一行与账本体耦合——可分 `runs/annex.rs`，待 annex 条规
//! 增生再动（现在拆是切正交设计面，无收益）。
//!
//! - 账面 = graph store 的 `discovery_leads` 表；身份 = (type, locator) 的
//!   `dedupe_key`（hash_parts 同源 sha1·16hex），表主键即幂等唯一键——
//!   同键任意状态再入账 → OR IGNORE 跳行（幂等）。
//! - 状态机：pending_approval →（审批缝端点 / L1 自治）approved →
//!   （collect 尾段按预算消费）consumed + yield_count；人工可改 rejected
//!   （一击终态，拒因留档）；适配器无映射的类型写 deferred。
//!   禁倒退路径（approve_transition / reject_transition 唯一裁决点）。
//! - fail-open 血脉（表形态）：入账/写回失败以 Err 面世，调用方
//!   （pipeline/collect 尾段）响铃吞错——丢账目不丢感知，但绝不静默。
//! - 摘要段 `summary_line` 是下轮 AI 上下文唯一消费者（移除实验体：不在则死）。

use serde::{Deserialize, Serialize};

use crate::episodes::hash_parts;
use crate::graph::store::{Store, StoreError};
use crate::models::Lead;

/// M4.x 账本文件名（迁移期源文件；现布局单房间产物根）。
/// `dedupe_key` recipe 域前缀（`_hash` 第一槽）。
pub const DEDUPE_KEY_PREFIX: &str = "m4x-lead";
/// `dedupe_key` 截断长度（`_hash` 第二参；与 evidence_id/episode_id 全产线同宽）。
pub const DEDUPE_KEY_LEN: usize = 16;
/// 摘要段 latest_consumed 最多回放的消费行数（kickoff 契约）。
pub const LATEST_CONSUMED_CAP: usize = 3;
/// 消费留痕 `resolution_note` 的长度上限（summary/端点面可人工浏览，不打爆单行）。
pub const RESOLUTION_NOTE_CAP: usize = 240;
/// audience 侧账本行 viewer_id 占位（leads 来自整体态势终局提交）。
pub const AUDIENCE_VIEWER_ID: &str = "audience";
/// 四型白名单的唯一真源（validators.rs 的 LEAD_TYPE_WHITELIST 指认此处）。
pub const LEAD_TYPES: [&str; 4] = ["search", "creator", "video", "room"];
/// annex 摘要里 latest_consumed 的 locator 展示宽度。
pub const ANNEX_LOCATOR_CAP: usize = 80;
/// 拒因自由文本上限（观点留档，不打爆账本行）。chip 白名单已删（删码刀6）——
/// 拒因的终极消费者是 LLM 回喂与人类回读，自然语言本就够用，无需人替 LLM 预分类。
pub const REJECT_NOTE_CAP: usize = 80;
/// reject_annex_line 最近注记的展示宽度（规格的「…40字截」）。
pub const REJECT_ANNEX_NOTE_CHARS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadStatus {
    PendingApproval,
    Approved,
    Consumed,
    Rejected,
}

/// 账本行（字段 = M4.x JSONL 冻结契约与 discovery_leads 列集的共同真源）。
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
    /// 拒绝提交的 chip（reject 端点白名单校验后落账）；
    /// `#[serde(default)]` 空数组缺省。
    #[serde(default)]
    pub reject_chips: Vec<String>,
    /// 拒绝注记（reject 端点自由文本；全空合法 = 空串）。
    #[serde(default)]
    pub reject_note: String,
}

/// `_hash(f"m4x-lead|{type}|{locator}", 16)`：可操作目标即身份，motivation 措辞不入。
pub fn dedupe_key(lead: &Lead) -> String {
    hash_parts(
        &[
            DEDUPE_KEY_PREFIX.to_string(),
            lead.lead_type.clone(),
            lead.locator.clone(),
        ],
        DEDUPE_KEY_LEN,
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
        reject_chips: Vec::new(),
        reject_note: String::new(),
    }
}

/// 入账新线索（幂等）：同 dedupe_key 任意状态已存在则跳行。返回实际入库行数。
/// Err 面世（FK 违约 / 库不可写），由调用方响铃吞纳。
pub fn record_leads(
    store: &Store,
    viewer_id: &str,
    run_id: &str,
    now: &str,
    leads: &[Lead],
) -> Result<usize, StoreError> {
    let rows: Vec<LedgerRow> = leads
        .iter()
        .map(|lead| pending_row(lead, viewer_id, run_id, now))
        .collect();
    let refs: Vec<&LedgerRow> = rows.iter().collect();
    store.insert_lead_rows(&refs, false)
}

/// 全账读面（写账序）——annex / overview / 审批缝的唯一数据源。
pub fn read_rows(store: &Store) -> Result<Vec<LedgerRow>, StoreError> {
    store.lead_rows()
}

/// G2-B 审批缝状态机辅助：`pending_approval → approved` 是 approve 通道唯一合法
/// 迁移（禁倒退纪律的程序面）。返回值 = 是否需要落盘改写：
/// - approved 重放 → Ok(false)：幂等，终态相同、表不动；
/// - consumed/rejected/deferred 源态 → Err（422 面错文：规则 + 当前源态）。
pub fn approve_transition(status: LeadStatus) -> Result<bool, String> {
    match status {
        LeadStatus::PendingApproval => Ok(true),
        LeadStatus::Approved => Ok(false),
        other => Err(format!(
            "状态机只许 pending_approval → approved；\
             当前状态 {}，不允许此迁移",
            status_name(other)
        )),
    }
}

/// reject 端点状态机辅助——`pending_approval → rejected` 是拒绝通道
/// 唯一合法迁移（一击终态：approved/consumed/deferred 源态一律 Err，禁倒退）。
/// 返回值 = 是否需要落盘改写：
/// - rejected 重放 → Ok(false)：幂等，终态相同、表不动（reject 端点据此决定
///   同参/空参返回同态、异参 422——终态不可改写）；
/// - 其余非 pending 源态 → Err（422 面错文：规则 + 当前源态）。
pub fn reject_transition(status: LeadStatus) -> Result<bool, String> {
    match status {
        LeadStatus::PendingApproval => Ok(true),
        LeadStatus::Rejected => Ok(false),
        other => Err(format!(
            "状态机只许 pending_approval → rejected；\
             当前状态 {}，不允许此迁移",
            status_name(other)
        )),
    }
}

/// 状态名的 serde snake_case 字面（错文/日志/表 status 列的唯一拼写源）。
pub fn status_name(status: LeadStatus) -> &'static str {
    match status {
        LeadStatus::PendingApproval => "pending_approval",
        LeadStatus::Approved => "approved",
        LeadStatus::Consumed => "consumed",
        LeadStatus::Rejected => "rejected",
    }
}

/// status 列字面 → 状态机枚举（表读面的唯一还原点）。
pub fn status_from_name(name: &str) -> Option<LeadStatus> {
    match name {
        "pending_approval" => Some(LeadStatus::PendingApproval),
        "approved" => Some(LeadStatus::Approved),
        "consumed" => Some(LeadStatus::Consumed),
        "rejected" => Some(LeadStatus::Rejected),
        _ => None,
    }
}

/// kickoff 契约摘要段。`viewer=None` → 全局行；`Some(v)` → 前缀 `viewer=… own_pending=…`。
/// by_type 只计未被 reject 的行（死账不喂回下轮）。
///
/// 账本是人工编辑面——
/// 1. by_type 键只走四型白名单，其余收拢进 `other`（手编毒行灌不进命题面）；
/// 2. latest_consumed 的 locator 截 ANNEX_LOCATOR_CAP=80（文本维度有封顶）。
pub fn summary_line(rows: &[LedgerRow], viewer: Option<&str>) -> String {
    let count = |status: LeadStatus| rows.iter().filter(|r| r.status == status).count();
    let mut by_type: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for row in rows.iter().filter(|r| r.status != LeadStatus::Rejected) {
        let name = if LEAD_TYPES.contains(&row.lead_type.as_str()) {
            row.lead_type.as_str()
        } else {
            "other"
        };
        *by_type.entry(name).or_insert(0) += 1;
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
                // 与 by_type 同一折叠：annex 文本面非白名单一律渲染 "other"
                "type": if LEAD_TYPES.contains(&r.lead_type.as_str()) { r.lead_type.clone() } else { "other".to_string() },
                "locator": r.locator.chars().take(ANNEX_LOCATOR_CAP).collect::<String>(),
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
        "{head} pending={} approved={} consumed={} rejected={} \
         by_type={{{by_type}}} yield_total={yield_total} latest_consumed={latest}",
        count(LeadStatus::PendingApproval),
        count(LeadStatus::Approved),
        count(LeadStatus::Consumed),
        count(LeadStatus::Rejected),
        latest = serde_json::to_string(&latest).expect("latest 可序列化"),
    )
}

// ---------------------------------------------------------------------------
// 消费回喂 annex（迭代细则 v1 §1）：summary_line 之外的事实密度段——
// 每 consumed lead 一纸「事实密度」账单：触达的观众画像 + M 条证据摘要，
// 行尾 episode_id 回链 lead→episode 链。幂等与单调漂移是铁律：
// 零新增 → 同库逐字节一致；变化只随 consumed 漂移。
// ---------------------------------------------------------------------------

/// 每个 consumed lead 最多列名刷新画像的观众数（细则：N ≤3 的姊妹 cap）。
/// 删码专项：原 ANNEX_FACTS_PER_LEAD 声明了 facts ≤3 但从未接进实现——
/// ANNEX_TOTAL_CHARS 总闸已是 annex 行数的唯一裁（每行过闸），单件 cap 属无消费者声明，删。
pub const ANNEX_VIEWERS_CAP: usize = 3;
/// 单行摘要字符上限：一句话密度（ANNEX_LOCATOR_CAP=80 的半格位）。
pub const ANNEX_SNIPPET_CHARS: usize = 40;
/// annex 全文总长度上限（防账本滚大吞 prompt 预算：超出即截尾、行边界断）。
/// 定标：典型重行（locator 24 截 + N=3 观众 + M=3 摘区段 + 回链锚）≈110–130 字，
/// LATEST_CONSUMED_CAP=3 行满载 ≈330–390——紧压 360：三条都活着时它刚好看不见，
/// 但把 prompt 预算拱出线它立即响。
pub const ANNEX_TOTAL_CHARS: usize = 360;

pub fn consumed_annex_lines(store: &Store, rows: &[LedgerRow]) -> Result<Vec<String>, StoreError> {
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0_usize;
    let consumed: Vec<&LedgerRow> = rows
        .iter()
        .filter(|r| r.status == LeadStatus::Consumed)
        .rev()
        .take(LATEST_CONSUMED_CAP)
        .collect();
    let consumed_total = rows
        .iter()
        .filter(|r| r.status == LeadStatus::Consumed)
        .count();
    for row in &consumed {
        let locator_short: String = row.locator.chars().take(24).collect();
        let facts = query_episode_annex(store, &row.dedupe_key)?;
        let mut viewer_counts: Vec<(String, i64)> = Vec::new();
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for fact in &facts {
            match seen.get(&fact.viewer_id) {
                Some(index) => viewer_counts[*index].1 += 1,
                None => {
                    seen.insert(fact.viewer_id.clone(), viewer_counts.len());
                    viewer_counts.push((fact.viewer_id.clone(), 1));
                }
            }
        }
        let viewers_label = if viewer_counts.is_empty() {
            "0 人".to_string()
        } else {
            let head = viewer_counts
                .iter()
                .take(ANNEX_VIEWERS_CAP)
                .map(|(uid, n)| format!("{uid}+{n}"))
                .collect::<Vec<_>>()
                .join("、");
            if viewer_counts.len() > ANNEX_VIEWERS_CAP {
                format!("{head}…{} 人", viewer_counts.len())
            } else {
                head
            }
        };
        let snippet_tail = facts
            .last()
            .map(|f| {
                format!(
                    "；末条《{}》",
                    f.snippet
                        .chars()
                        .take(ANNEX_SNIPPET_CHARS)
                        .collect::<String>()
                )
            })
            .unwrap_or_else(|| "；尚无证据落图".to_string());
        let line = format!(
            "[lead_fact] {}:{} → 画像 {}（总证据 {}）{} 〔回链 {}〕",
            row.lead_type,
            locator_short,
            viewers_label,
            facts.len(),
            snippet_tail,
            facts
                .last()
                .map(|f| f.episode_tail.clone())
                .unwrap_or_else(|| "—".to_string()),
        );
        // 总长闸：行边界断——不剖半行；断则追加封顶句且终盘。
        if total + line.chars().count() > ANNEX_TOTAL_CHARS {
            lines.push(format!(
                "[lead_fact] …（共 {consumed_total} 条已消费，预算封因为其截断）"
            ));
            break;
        }
        total += line.chars().count();
        lines.push(line);
    }
    Ok(lines)
}

/// 拒绝回喂线：被拒行是平台事实，喂回下轮提示面助 Agent 定向（人拒一条 =
/// 「此路不通」的最强信号）。零被拒行 → Ok(None)（提示面不注入一字节）。
///
/// 自由文本形态：最多最近 3 条 `locator：拒因`（无拒因落「无拒因」）。
pub fn reject_annex_line(store: &Store) -> Result<Option<String>, StoreError> {
    let rows = store.lead_rows()?;
    let rejected: Vec<&LedgerRow> = rows
        .iter()
        .filter(|r| r.status == LeadStatus::Rejected)
        .collect();
    if rejected.is_empty() {
        return Ok(None);
    }
    let mut lines = vec![format!("[lead_reject] 上轮被拒 {} 条：", rejected.len())];
    for row in rejected.iter().rev().take(3) {
        let locator = row
            .locator
            .chars()
            .take(ANNEX_LOCATOR_CAP)
            .collect::<String>();
        let note = row.reject_note.trim();
        let note = note
            .chars()
            .take(REJECT_ANNEX_NOTE_CHARS)
            .collect::<String>();
        let reason = if note.is_empty() {
            "无拒因".to_string()
        } else {
            note
        };
        lines.push(format!("- {locator}：{reason}"));
    }
    Ok(Some(lines.join("\n")))
}

struct AnnexFact {
    viewer_id: String,
    snippet: String,
    episode_tail: String,
}

/// episodes.lead_id = discovery_leads.dedupe_key（FK 反向读面）。
/// 排序 = episode_id 字典序：确定性锚（同一库零改变同输出——松物料名需求）。
fn query_episode_annex(store: &Store, dedupe_key: &str) -> Result<Vec<AnnexFact>, StoreError> {
    let mut stmt = store
        .conn
        .prepare(
            "SELECT episode_id,viewer_id,title,fields_json FROM episodes \
             WHERE lead_id = ? ORDER BY episode_id ASC",
        )
        .map_err(|err| StoreError::Repo(format!("annex 查询准备失败: {err}")))?;
    let rows = stmt
        .query_map(rusqlite::params![dedupe_key], |row| {
            let episode_id: String = row.get(0)?;
            let viewer_id: String = row.get(1)?;
            let title: String = row.get(2)?;
            let fields_json: String = row.get(3)?;
            Ok((episode_id, viewer_id, title, fields_json))
        })
        .map_err(|err| StoreError::Repo(format!("annex 查询失败: {err}")))?;
    let mut out = Vec::new();
    for row in rows {
        let (episode_id, viewer_id, title, fields_json) =
            row.map_err(|err| StoreError::Repo(format!("annex 行读失败: {err}")))?;
        out.push(AnnexFact {
            viewer_id,
            snippet: annex_snippet_of(&title, &fields_json),
            episode_tail: {
                let chars: Vec<char> = episode_id.chars().collect();
                chars.iter().skip(chars.len().saturating_sub(12)).collect()
            },
        });
    }
    Ok(out)
}

/// 文本摘要：title 优先；空则首条 fields.text；真无 → 明确「〔无文本〕」（不造句）。
fn annex_snippet_of(title: &str, fields_json: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    serde_json::from_str::<serde_json::Value>(fields_json)
        .ok()
        .and_then(|fields| {
            fields
                .as_array()?
                .first()?
                .get("text")?
                .as_str()
                .map(str::to_string)
        })
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "〔无文本〕".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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

    fn row(key: &str, lead_type: &str, status: LeadStatus) -> LedgerRow {
        LedgerRow {
            dedupe_key: key.into(),
            lead_type: lead_type.into(),
            locator: format!("loc-{key}"),
            motivation: "m".into(),
            expected_signal: "s".into(),
            priority: "high".into(),
            evidence_ids: vec![],
            viewer_id: "u".into(),
            first_seen_run_id: "run:a".into(),
            created_at: "t".into(),
            status,
            yield_count: 0,
            resolution_note: String::new(),
            reject_chips: Vec::new(),
            reject_note: String::new(),
        }
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
                DEDUPE_KEY_LEN
            )
        );
        // 自参防漂移：截断宽度的独立字面钉
        assert_eq!(DEDUPE_KEY_LEN, 16);
        assert_eq!(dedupe_key(&row).len(), 16);
        // (type, locator) 是身份：motivation/evidence 不入键
        let mut same = row.clone();
        same.motivation = "另一措辞".to_string();
        same.evidence_ids = vec![];
        assert_eq!(dedupe_key(&row), dedupe_key(&same));
        // type 或 locator 变 → 身份变
        let other = lead("creator", "BV1aa411c7mD");
        assert_ne!(dedupe_key(&row), dedupe_key(&other));
    }

    /// JSONL/.bak 行字段序 = 冻结契约书写序（serde 声明序隐式
    /// 保证过域，不做断言则一次字段重排即静默违约——归档本被线下工具消费）。
    #[test]
    fn ledger_row_field_order_pinned() {
        let row = pending_row(&lead("video", "BV1"), "u", "run:a", "t");
        let line = serde_json::to_string(&row).unwrap();
        let keys = [
            "dedupe_key",
            "\"type\"",
            "locator",
            "motivation",
            "expected_signal",
            "priority",
            "evidence_ids",
            "viewer_id",
            "first_seen_run_id",
            "created_at",
            "status",
            "yield_count",
            "resolution_note",
            "reject_chips",
            "reject_note",
        ];
        let positions: Vec<usize> = keys
            .iter()
            .map(|key| line.find(key).unwrap_or_else(|| panic!("缺键 {key}")))
            .collect();
        for window in positions.windows(2) {
            assert!(window[0] < window[1], "字段序漂移：{line}");
        }
    }

    /// 空账本摘要形态钉——零账本状态下 pipeline annex 依赖此形态。
    #[test]
    fn empty_ledger_summary_pinned() {
        assert_eq!(
            summary_line(&[], None),
            "[lead_ledger] pending=0 approved=0 consumed=0 rejected=0 \
             by_type={} yield_total=0 latest_consumed=[]"
        );
    }

    /// 守卫解析：好行进出全等；坏行载行号 Err（迁移停火面的措辞钉）。
    #[test]
    fn summary_line_matches_frozen_contract() {
        let mut rows = vec![
            row("k1", "video", LeadStatus::PendingApproval),
            row("k2", "video", LeadStatus::PendingApproval),
            row("k3", "creator", LeadStatus::PendingApproval),
        ];
        rows[1].status = LeadStatus::Approved;
        rows[0].status = LeadStatus::Consumed;
        rows[0].yield_count = 5;
        let global = summary_line(&rows, None);
        assert_eq!(
            global,
            "[lead_ledger] pending=1 approved=1 consumed=1 rejected=0 \
             by_type={creator: 1, video: 2} yield_total=5 latest_consumed=[{\"type\":\"video\",\"locator\":\"loc-k1\",\"yield_count\":5}]"
        );
        let scoped = summary_line(&rows, Some("u"));
        assert!(
            scoped.starts_with("[lead_ledger] viewer=u own_pending=1 | pending=1 approved=1"),
            "{scoped}"
        );
        // rejected 不进 by_type；approved 照计
        rows[1].status = LeadStatus::Rejected;
        let line = summary_line(&rows, None);
        assert_eq!(
            line,
            "[lead_ledger] pending=1 approved=0 consumed=1 rejected=1 \
             by_type={creator: 1, video: 1} yield_total=5 latest_consumed=[{\"type\":\"video\",\"locator\":\"loc-k1\",\"yield_count\":5}]"
        );
    }

    #[test]
    fn latest_consumed_capped_at_three() {
        let mut rows: Vec<LedgerRow> = (0..5)
            .map(|i| row(&format!("k{i}"), "video", LeadStatus::PendingApproval))
            .collect();
        for (i, entry) in rows.iter_mut().enumerate() {
            entry.status = LeadStatus::Consumed;
            entry.yield_count = i as i64;
        }
        let line = summary_line(&rows, None);
        // 写账序倒取 3：k4→k3→k2（= 旧文件序的表同构物）
        assert!(
            line.contains(
                "latest_consumed=[{\"type\":\"video\",\"locator\":\"loc-k4\",\"yield_count\":4},{\"type\":\"video\",\"locator\":\"loc-k3\",\"yield_count\":3},{\"type\":\"video\",\"locator\":\"loc-k2\",\"yield_count\":2}]"
            ),
            "{line}"
        );
        assert!(line.contains("yield_total=10"));
    }

    /// 手编毒行进不了命题面——非白名单 type 收拢进
    /// "other" 桶；长 locator 在 latest 里截断到上限。
    #[test]
    fn annex_folds_unknown_types_and_caps_locator() {
        let mut poisoned = row("k1", "video", LeadStatus::Consumed);
        poisoned.lead_type = "w;&#}%@注入".to_string();
        poisoned.locator = "L".repeat(200);
        poisoned.yield_count = 1;
        let line = summary_line(&[poisoned.clone()], None);
        assert!(line.contains("by_type={other: 1}"), "{line}");
        assert!(!line.contains("注入"), "{line}");
        let displayed = &poisoned
            .locator
            .chars()
            .take(ANNEX_LOCATOR_CAP)
            .collect::<String>();
        assert!(line.contains(displayed), "{line}");
        assert!(!line.contains(&"L".repeat(120)), "{line}");
        assert_eq!(ANNEX_LOCATOR_CAP, 80);
    }

    /// 四型真源一致性钉：validators 的 LEAD_TYPE_WHITELIST 指认 leads::LEAD_TYPES
    /// （类型闸声明单源化）。
    #[test]
    fn lead_types_single_source_pinned() {
        assert_eq!(crate::agent::validators::LEAD_TYPE_WHITELIST, LEAD_TYPES);
    }

    /// status 面双拼写源：name ⇄ 枚举 全往返（表 status 列与 serde 的同源钉）。
    #[test]
    fn status_name_roundtrip_pinned() {
        for status in [
            LeadStatus::PendingApproval,
            LeadStatus::Approved,
            LeadStatus::Consumed,
            LeadStatus::Rejected,
        ] {
            assert_eq!(status_from_name(status_name(status)), Some(status));
        }
        assert_eq!(status_from_name("nonsense"), None);
    }

    /// reject 状态机——pending→Ok(true) 落盘；rejected 重放→Ok(false)（幂等
    /// 终态、表不动）；approved/consumed 源态 → Err（422 规则错文）。
    /// 与 approve 同构：无倒退路径、错文带当前源态。
    #[test]
    fn reject_transition_over_table() {
        for status in [LeadStatus::Approved, LeadStatus::Consumed] {
            let err = reject_transition(status).unwrap_err();
            assert!(err.contains("pending_approval → rejected"), "{err}");
            assert!(err.contains(status_name(status)), "错文应带当前源态：{err}");
        }
        assert_eq!(reject_transition(LeadStatus::PendingApproval), Ok(true));
        assert_eq!(reject_transition(LeadStatus::Rejected), Ok(false));
        // 幂等面：rejected 重放与 approve 的 approved 重放同构
        assert_eq!(approve_transition(LeadStatus::Approved), Ok(false));
    }

    /// 上限常量字面钉（防误改——前端与服务端共享镜像）。
    #[test]
    fn reject_constants_pinned() {
        assert_eq!(REJECT_NOTE_CAP, 80);
        assert_eq!(REJECT_ANNEX_NOTE_CHARS, 40);
    }

    /// reject_annex_line 自由文本形态：首行计数 + 最近 3 条 `locator：拒因`
    /// （无拒因落「无拒因」；拒绝行倒序=写账序近者在前；REJECT_ANNEX_NOTE_CHARS 截字）。
    #[test]
    fn reject_annex_line_free_text_recent_first() {
        let store = Store::open(Path::new(":memory:")).expect("store");
        store.begin_run_fixed("run:a", "t0", "model").expect("run");
        let mut r1 = row("k1", "video", LeadStatus::Rejected);
        r1.reject_note = "阈值太低，覆盖太宽".into();
        let mut r2 = row("k2", "creator", LeadStatus::Rejected);
        r2.reject_note = "长".repeat(60);
        let r3 = row("k3", "room", LeadStatus::Rejected); // 无拒因形态混入
        let r4 = row("k4", "room", LeadStatus::PendingApproval); // 非 rejected 不沾线
        store
            .insert_lead_rows(&[&r1, &r2, &r3, &r4], false)
            .expect("insert");
        let line = reject_annex_line(&store).unwrap().expect("有被拒行");
        let note = "长".repeat(40);
        assert_eq!(
            line,
            format!(
                "[lead_reject] 上轮被拒 3 条：\n- loc-k3：无拒因\n- loc-k2：{note}\n- loc-k1：阈值太低，覆盖太宽"
            ),
            "{line}"
        );
        assert_eq!(line.matches("\n- ").count(), 3, "恰好近 3 条：{line}");
    }

    /// 零被拒行 → None（零字节未响应面）且同库两次读逐字节一致（幂等钉）。
    #[test]
    fn reject_annex_line_none_and_byte_identical() {
        let store = Store::open(Path::new(":memory:")).expect("store");
        store.begin_run_fixed("run:a", "t0", "model").expect("run");
        store
            .insert_lead_rows(&[&row("k1", "video", LeadStatus::PendingApproval)], false)
            .expect("insert");
        assert_eq!(reject_annex_line(&store).unwrap(), None);
        assert_eq!(reject_annex_line(&store).unwrap(), None);
        // 造一条 rejected 后再钉两次读逐字节一致
        let rejected = row("k2", "creator", LeadStatus::Rejected);
        store
            .insert_lead_rows(&[&rejected], false)
            .expect("insert k2");
        let first = reject_annex_line(&store).unwrap().expect("line");
        let second = reject_annex_line(&store).unwrap().expect("line");
        assert_eq!(first, second);
        assert_eq!(first, "[lead_reject] 上轮被拒 1 条：\n- loc-k2：无拒因");
    }
}
