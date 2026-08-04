//! M4.x 消费通道（kickoff D5/D6 + design §16 薄切条款）：collect 尾段按预算消费
//! 人工批准的 leads。预算 = 尝试次数（attempt 计次，成败各记一次——失败行下轮重试）；
//! deferred 不烧预算。消费产物落袋是 G2 账目消费器的管辖，本通道只记数与状态。

use crate::bilibili::BilibiliClient;
use crate::leads::{LeadStatus, LedgerRow, ledger_path, read_ledger_guarded, rewrite_ledger};
use std::path::Path;

/// 单行为产抓取时每一种类型的条目快照上限（kickoff D5：search/creator 各自 8）。
pub const LEAD_RESULT_LIMIT: i64 = 8;
/// search 型消费的排序口径（与 ResearchService 默认一致：综合排序）。
pub const LEAD_SEARCH_ORDER: &str = "totalrank";

/// 真实抓取映射（kickoff D5）。room/未知类型天生缺席 → 上层归 deferred。
///
/// MXA-3（r7）：本地图是账本被人工编辑后的第一站——入口做 locator 卫生
/// （trim + 空白拒），否则全空格 search 会被下游 trim 成空返回、假证 consumed
/// （禁倒退终态永可不再入），尾空格行每轮烧预算重试至死。
pub fn fetch_lead_yield(client: &mut BilibiliClient, row: &LedgerRow) -> Result<i64, String> {
    let locator = row.locator.trim();
    if locator.is_empty() {
        return Err("lead locator 空白（账本可能被手工编辑）".to_string());
    }
    match row.lead_type.as_str() {
        "search" => client
            .search_videos(locator, LEAD_RESULT_LIMIT, LEAD_SEARCH_ORDER)
            .map(|items| items.len() as i64)
            .map_err(|err| err.to_string()),
        "creator" => client
            .videos(locator, LEAD_RESULT_LIMIT)
            .map(|items| items.len() as i64)
            .map_err(|err| err.to_string()),
        "video" => client
            .video_detail(locator)
            .map(|_detail| 1)
            .map_err(|err| err.to_string()),
        other => Err(format!("no fetcher for lead type {other}")),
    }
}

/// G2-B（工作项 3）：L1 自动批准在账本行的留痕字面（「L1 自动」是契约子串）。
pub const L1_AUTO_NOTE: &str = "L1 自动批准（collection.leads_autonomy=1）";

/// G2-B 自治 L1（collection.leads_autonomy=1 时由 collect 尾段在预算消费前调用）：
/// 把符合谓词的 pending 行就地迁 `Approved` 并记 `resolution_note=L1_AUTO_NOTE`，
/// 随后照常走 `consume_approved_leads` 预算内消费。
///
/// 谓词只碰账本既有字段（LedgerRow.lead_type / locator，schema 侦察面）：
/// - 类型限 `creator`/`search`（video/room 永远人工域）；
/// - `creator` 目标 uid（locator 即 uid，validators 冻结数字形）不得在本房间
///   既有名册（viewers/*.json 文件名 ∪ 主播 uid——采在册者零增量）；`search`
///   无目标 uid，名册闸天然不适用。
///
/// autonomy ≤ 0 → 秒返 0（L0 现状纯人工，一字不动，账本不读不写）。
/// MXA-1/MXA-4 同族：坏行在场停火响铃；写回失败响铃返回 0。
pub fn auto_approve_pending_leads(
    output_dir: &Path,
    roster: &std::collections::BTreeSet<String>,
    autonomy: i64,
    emit: &mut dyn FnMut(&str),
) -> usize {
    if autonomy <= 0 {
        return 0;
    }
    let mut rows = match read_ledger_guarded(&ledger_path(output_dir)) {
        Ok(rows) => rows,
        Err(err) => {
            emit(&format!(
                "[LEADS] 账本不可读或含坏行，L1 自动批准停火：{err}"
            ));
            return 0;
        }
    };
    if !rows
        .iter()
        .any(|row| row.status == LeadStatus::PendingApproval)
    {
        return 0;
    }
    let mut flipped = 0_usize;
    for row in rows.iter_mut() {
        if row.status != LeadStatus::PendingApproval {
            continue;
        }
        let eligible = match row.lead_type.as_str() {
            "creator" => !roster.contains(row.locator.trim()),
            "search" => true,
            _ => false,
        };
        if !eligible {
            continue;
        }
        row.status = LeadStatus::Approved;
        row.resolution_note = L1_AUTO_NOTE.to_string();
        flipped += 1;
    }
    if flipped == 0 {
        return 0;
    }
    if let Err(err) = rewrite_ledger(output_dir, &rows) {
        emit(&format!("[LEADS] L1 自动批准写回失败：{err}"));
        return 0;
    }
    flipped
}

/// 按预算消费账本：只碰 `approved` 行；返回消费成功行数。
/// fetch 失败 → 行保持 approved 并记 `resolution_note`（下轮重试）；预算 0 → 秒返。
///
/// MXA-4：账本读写失败响铃（emit），绝不静默；rewrite 失败返回 0（不虚报
/// leads_consumed）。MXA-1：读用守卫（坏行在场时整轮消费停火，保不可解析行不被
/// 重写丢弃）。
pub fn consume_approved_leads(
    output_dir: &Path,
    budget: i64,
    fetch: &mut dyn FnMut(&LedgerRow) -> Result<i64, String>,
    emit: &mut dyn FnMut(&str),
) -> usize {
    if budget <= 0 {
        return 0;
    }
    let mut rows = match read_ledger_guarded(&ledger_path(output_dir)) {
        Ok(rows) => rows,
        Err(err) => {
            emit(&format!("[LEADS] 账本不可读或含坏行，本轮消费停火：{err}"));
            return 0;
        }
    };
    if !rows.iter().any(|row| row.status == LeadStatus::Approved) {
        return 0;
    }
    let mut attempts = 0_i64;
    let mut consumed = 0_usize;
    let mut dirty = false;
    for row in rows.iter_mut() {
        if row.status != LeadStatus::Approved {
            continue;
        }
        match row.lead_type.as_str() {
            "room" => {
                row.status = LeadStatus::Deferred;
                row.resolution_note = "room 型 lead 无适配器映射（M4.x 薄切不扩端点）".into();
                dirty = true;
                continue;
            }
            other if !["search", "creator", "video"].contains(&other) => {
                row.status = LeadStatus::Deferred;
                row.resolution_note = format!("未知类型 {other}（账本可能被手工编辑）");
                dirty = true;
                continue;
            }
            _ => {}
        }
        if attempts >= budget {
            break;
        }
        attempts += 1;
        match fetch(row) {
            Ok(yield_count) => {
                row.status = LeadStatus::Consumed;
                row.yield_count = yield_count;
                row.resolution_note.clear();
                consumed += 1;
            }
            Err(err) => {
                row.resolution_note = err
                    .chars()
                    .take(crate::leads::RESOLUTION_NOTE_CAP)
                    .collect();
            }
        }
        dirty = true;
    }
    if dirty && let Err(err) = rewrite_ledger(output_dir, &rows) {
        // MXA-4：写不回就不能记成功（内存态已改但盘面未变，返回 0 保 leads_consumed 真值）
        emit(&format!(
            "[LEADS] 账本写回失败，本轮消费结果未持久化：{err}"
        ));
        return 0;
    }
    consumed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bilibili::BilibiliClient;
    use crate::leads::{self, LedgerRow, read_ledger};

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("m4x-consume-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        }
    }

    fn write(dir: &Path, rows: &[LedgerRow]) {
        let text = rows
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(leads::ledger_path(dir), text).unwrap();
    }

    /// 预算计次 + 状态落袋 + 持久化：1 consumed + 1 budget 截断 + 1 pending 未触碰。
    #[test]
    fn budget_caps_attempts_and_writes_back() {
        let dir = tmp("cap");
        write(
            &dir,
            &[
                row("k1", "video", LeadStatus::Approved),
                row("k2", "video", LeadStatus::Approved),
                row("k3", "search", LeadStatus::PendingApproval),
            ],
        );
        let n = consume_approved_leads(&dir, 1, &mut |_row| Ok(5), &mut |_: &str| {});
        assert_eq!(n, 1);
        let back = read_ledger(&leads::ledger_path(&dir));
        assert_eq!(back[0].status, LeadStatus::Consumed);
        assert_eq!(back[0].yield_count, 5);
        assert_eq!(back[1].status, LeadStatus::Approved, "预算只够一次尝试");
        assert_eq!(
            back[2].status,
            LeadStatus::PendingApproval,
            "非 approved 不动"
        );
        // 预算截断的余行留到下一轮消费（跨 run 续抓）；抓尽后再轮 = 零动作幂等。
        assert_eq!(
            consume_approved_leads(&dir, 5, &mut |_r| Ok(3), &mut |_: &str| {}),
            1
        );
        assert_eq!(read_ledger(&leads::ledger_path(&dir))[1].yield_count, 3);
        assert_eq!(
            consume_approved_leads(&dir, 5, &mut |_r| Ok(1), &mut |_: &str| {}),
            0
        );
    }

    /// 抓取失败 = 烧预算但保持 approved + 留痕，下轮重试；坏行不炸。
    #[test]
    fn failure_keeps_approved_with_note() {
        let dir = tmp("fail");
        write(&dir, &[row("k1", "video", LeadStatus::Approved)]);
        let n =
            consume_approved_leads(&dir, 2, &mut |_r| Err("biu".repeat(200)), &mut |_: &str| {});
        assert_eq!(n, 0);
        let back = read_ledger(&leads::ledger_path(&dir));
        assert_eq!(back[0].status, LeadStatus::Approved);
        assert_eq!(
            back[0].resolution_note.chars().count(),
            crate::leads::RESOLUTION_NOTE_CAP,
            "留痕截断"
        );
    }

    /// room/手编坏类型 → deferred 不烧预算。
    #[test]
    fn deferred_types_burn_no_budget() {
        let dir = tmp("defer");
        write(
            &dir,
            &[
                row("k1", "room", LeadStatus::Approved),
                row("k2", "nonsense", LeadStatus::Approved),
            ],
        );
        let n = consume_approved_leads(&dir, 1, &mut |_r| Ok(9), &mut |_: &str| {});
        assert_eq!(n, 0);
        let back = read_ledger(&leads::ledger_path(&dir));
        assert_eq!(back[0].status, LeadStatus::Deferred);
        assert!(back[0].resolution_note.contains("无适配器映射"));
        assert_eq!(back[1].status, LeadStatus::Deferred);
        assert!(back[1].resolution_note.contains("未知类型"));
    }

    /// 预算 0 = 完全休眠（默认文化），账目原样。
    #[test]
    fn zero_budget_sleeps() {
        let dir = tmp("zero");
        write(&dir, &[row("k1", "video", LeadStatus::Approved)]);
        assert_eq!(
            consume_approved_leads(&dir, 0, &mut |_r| Ok(9), &mut |_: &str| {}),
            0
        );
        assert_eq!(
            read_ledger(&leads::ledger_path(&dir))[0].status,
            LeadStatus::Approved
        );
    }

    /// MXA-4（r4-G-4）：写回失败 → 响铃 + 返回 0（不谎报 leads_consumed）。
    /// 确定性触发：把 `leads.jsonl.tmp` 预建成目录 → tmp 创建必失败。
    #[test]
    fn rewrite_failure_rings_and_reports_zero() {
        let dir = tmp("ring");
        write(&dir, &[row("k1", "video", LeadStatus::Approved)]);
        std::fs::create_dir_all(leads::ledger_path(&dir).with_extension("jsonl.tmp")).unwrap();
        let mut rings: Vec<String> = Vec::new();
        let n = consume_approved_leads(&dir, 2, &mut |_r| Ok(9), &mut |m: &str| {
            rings.push(m.to_string())
        });
        assert_eq!(n, 0, "写不回就不计成功");
        assert!(
            rings.iter().any(|m| m.contains("写回失败")),
            "rings={rings:?}"
        );
        // 盘面没动：k1 仍是 approved（可重试）
        assert_eq!(
            read_ledger(&leads::ledger_path(&dir))[0].status,
            LeadStatus::Approved
        );
    }

    /// MXA-1/MXA-4：账本含坏行 → 整轮消费停火 + 响铃（坏行不被重写丢弃）。
    #[test]
    fn corrupt_ledger_stops_consumption_with_ring() {
        let dir = tmp("guard");
        let good = row("k1", "video", LeadStatus::Approved);
        let text = format!("{}\nnot json\n", serde_json::to_string(&good).unwrap());
        std::fs::write(leads::ledger_path(&dir), &text).unwrap();
        let mut rings: Vec<String> = Vec::new();
        let n = consume_approved_leads(&dir, 2, &mut |_r| Ok(1), &mut |m: &str| {
            rings.push(m.to_string())
        });
        assert_eq!(n, 0);
        assert!(rings.iter().any(|m| m.contains("停火")), "{rings:?}");
        assert_eq!(
            std::fs::read_to_string(leads::ledger_path(&dir)).unwrap(),
            text,
            "停火时账本文本不动"
        );
    }

    /// MXA-3（r7）：空白 locator 在任何网络请求前被拒（不会假证 consumed）。
    #[test]
    fn blank_locator_rejected_before_any_request() {
        let mut client = BilibiliClient::with_origin(
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
            "SESSDATA=test",
            0.0,
            5.0,
        )
        .unwrap();
        for (lead_type, locator) in [("search", "   "), ("video", ""), ("creator", "\t")] {
            let mut candidate = row("k1", lead_type, LeadStatus::Approved);
            candidate.locator = locator.to_string();
            let err = fetch_lead_yield(&mut client, &candidate).unwrap_err();
            assert!(err.contains("空白"), "{lead_type}: {err}");
        }
        assert_eq!(client.request_count(), 0, "卫生闸前不得发任何请求");
    }

    /// 消费后下轮摘要段读得到 yield（kickoff「下轮 AI 上下文」全环闭合点）。
    #[test]
    fn summary_reflects_consumed_rows() {
        let dir = tmp("sum");
        write(&dir, &[row("k1", "video", LeadStatus::Approved)]);
        consume_approved_leads(&dir, 1, &mut |_r| Ok(1), &mut |_: &str| {});
        let line = crate::leads::summary_line(&read_ledger(&leads::ledger_path(&dir)), None);
        assert_eq!(
            line,
            "[lead_ledger] pending=0 approved=0 consumed=1 rejected=0 deferred=0 \
             by_type={video: 1} yield_total=1 latest_consumed=[{\"type\":\"video\",\"locator\":\"loc-k1\",\"yield_count\":1}]",
            "{line}"
        );
    }

    // -----------------------------------------------------------------------
    // G2-B（工作项 3）：L1 自治自动批准钉团
    // -----------------------------------------------------------------------

    fn roster(ids: &[&str]) -> std::collections::BTreeSet<String> {
        ids.iter().map(ToString::to_string).collect()
    }

    /// L0 一字不动：autonomy=0 → 秒返 0，账本字节级原样（预算再大也无动作）。
    #[test]
    fn l0_autonomy_zero_sleeps_byte_identical() {
        let dir = tmp("l0");
        write(&dir, &[row("k1", "creator", LeadStatus::PendingApproval)]);
        let before = std::fs::read_to_string(leads::ledger_path(&dir)).unwrap();
        let n = auto_approve_pending_leads(&dir, &roster(&[]), 0, &mut |_: &str| {
            panic!("L0 不得发任何响铃")
        });
        assert_eq!(n, 0);
        assert_eq!(
            std::fs::read_to_string(leads::ledger_path(&dir)).unwrap(),
            before,
            "L0 账本字节级不动"
        );
    }

    /// L1 正当事：creator（目标 uid 不在册）+ search pending → 迁 Approved +
    /// resolution_note 记「L1 自动」；approved/rejected/consumed 行不被触碰。
    /// 重放幂等：无 pending 可批 → 0 且账本不动。
    #[test]
    fn l1_auto_approves_eligible_pending_rows_only() {
        let dir = tmp("l1");
        write(
            &dir,
            &[
                row("k-new-creator", "creator", LeadStatus::PendingApproval),
                row("k-search", "search", LeadStatus::PendingApproval),
                row("k-approved", "video", LeadStatus::Approved),
                row("k-rejected", "search", LeadStatus::Rejected),
                row("k-consumed", "creator", LeadStatus::Consumed),
            ],
        );
        // 名册：loc-k-new-creator 不在册（loc- 前缀由 row() 生成，名册只放别的 uid）。
        let n = auto_approve_pending_leads(&dir, &roster(&["1001", "9001"]), 1, &mut |_: &str| {});
        assert_eq!(n, 2, "creator + search 各迁一条");
        let back = read_ledger(&leads::ledger_path(&dir));
        for key in ["k-new-creator", "k-search"] {
            let hit = back.iter().find(|r| r.dedupe_key == key).unwrap();
            assert_eq!(hit.status, LeadStatus::Approved, "{key}");
            assert!(
                hit.resolution_note.contains("L1 自动"),
                "{key} 须记 L1 自动痕：{hit:?}"
            );
        }
        // 非 pending 行分毫不动（状态机单行道不被 L1 倒车）
        for (key, status) in [
            ("k-approved", LeadStatus::Approved),
            ("k-rejected", LeadStatus::Rejected),
            ("k-consumed", LeadStatus::Consumed),
        ] {
            let hit = back.iter().find(|r| r.dedupe_key == key).unwrap();
            assert_eq!(hit.status, status, "{key}");
            assert!(hit.resolution_note.is_empty(), "{key}");
        }
        // 重放幂等：再批一轮 = 0 动作，账本字节不动
        let settled = std::fs::read_to_string(leads::ledger_path(&dir)).unwrap();
        let n = auto_approve_pending_leads(&dir, &roster(&[]), 1, &mut |_: &str| {});
        assert_eq!(n, 0);
        assert_eq!(
            std::fs::read_to_string(leads::ledger_path(&dir)).unwrap(),
            settled
        );
    }

    /// L1 谓词拒位：video/room 型 pending 永远人工域；creator 目标 uid 已在册
    /// （重复采集无增量）同样不批——整账无一人动，不写盘。
    #[test]
    fn l1_predicate_rejects_video_room_and_in_roster_creator() {
        let dir = tmp("l1-no");
        write(
            &dir,
            &[
                row("k-video", "video", LeadStatus::PendingApproval),
                row("k-room", "room", LeadStatus::PendingApproval),
                row("k-existing", "creator", LeadStatus::PendingApproval),
            ],
        );
        let before = std::fs::read_to_string(leads::ledger_path(&dir)).unwrap();
        // 名册含 loc-k-existing（row() 的 locator = "loc-{key}"）
        let n = auto_approve_pending_leads(
            &dir,
            &roster(&["loc-k-existing", "9001"]),
            1,
            &mut |_: &str| {},
        );
        assert_eq!(n, 0, "谓词全拒 → 零动作");
        assert_eq!(
            std::fs::read_to_string(leads::ledger_path(&dir)).unwrap(),
            before,
            "全拒时不写账本"
        );
    }

    /// MXA-1 同族：坏行在场 → L1 停火 + 响铃，账本原文不动。
    #[test]
    fn l1_corrupt_ledger_stop_fire_with_ring() {
        let dir = tmp("l1-guard");
        let good = row("k1", "creator", LeadStatus::PendingApproval);
        let text = format!("{}\nnot json\n", serde_json::to_string(&good).unwrap());
        std::fs::write(leads::ledger_path(&dir), &text).unwrap();
        let mut rings: Vec<String> = Vec::new();
        let n = auto_approve_pending_leads(&dir, &roster(&[]), 1, &mut |m: &str| {
            rings.push(m.to_string())
        });
        assert_eq!(n, 0);
        assert!(rings.iter().any(|m| m.contains("停火")), "{rings:?}");
        assert_eq!(
            std::fs::read_to_string(leads::ledger_path(&dir)).unwrap(),
            text
        );
    }
}
