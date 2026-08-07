//! M4.x 消费通道的表形态（G2，design §9.2 行 254）：collect 尾段按预算消费
//! discovery_leads 表的 approved 行。预算 = 尝试次数（attempt 计次，成败各记一次
//! ——失败行下轮重试）；deferred 不烧预算。消费产物落袋是 G2 账目消费器的管辖，
//! 本通道只记数与状态。
//!
//! 体积备书：超 500 线主因 = 六种线索例行的逐种落袋体 + 测试加深 60%+；
//! 不拆线索消费器，和 JSONL 时态的单口记账语义保持一卷。
//!
//! 写回纪律（JSONL rewrite 的表同构物）：每行 UPDATE 即刻落库并即时响铃记数
//! ——不虚报 leads_consumed（表形态：写不回的行不计成功）。

use crate::bilibili::BilibiliClient;
use crate::graph::store::Store;
use crate::leads::{LeadStatus, LedgerRow};

/// 单行为产抓取时每一种类型的条目快照上限（search/creator 各自 8）。
pub const LEAD_RESULT_LIMIT: i64 = 8;
/// search 型消费的排序口径（与 ResearchService 默认一致：综合排序）。
pub const LEAD_SEARCH_ORDER: &str = "totalrank";

/// 真实抓取映射。room/未知类型天生缺席 → 上层归 deferred。
///
/// 本地图是账本被人工编辑后的第一站——入口做 locator 卫生
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

/// 随后照常走 `consume_approved_leads` 预算内消费。
///
/// 按预算消费账本：只碰 `approved` 行；返回消费成功行数。
/// fetch 失败 → 行保持 approved 并记 `resolution_note`（下轮重试）；预算 0 → 秒返。
///
/// 账本读写失败响铃（emit），绝不静默；写不回的行不计成功
/// （不虚报 leads_consumed）。
pub fn consume_approved_leads(
    store: &Store,
    budget: i64,
    fetch: &mut dyn FnMut(&LedgerRow) -> Result<i64, String>,
    emit: &mut dyn FnMut(&str),
) -> usize {
    if budget <= 0 {
        return 0;
    }
    let mut rows = match crate::leads::read_rows(store) {
        Ok(rows) => rows,
        Err(err) => {
            emit(&format!("[LEADS] 账本不可读，本轮消费停火：{err}"));
            return 0;
        }
    };
    if !rows.iter().any(|row| row.status == LeadStatus::Approved) {
        return 0;
    }
    let mut attempts = 0_i64;
    let mut consumed = 0_usize;
    let persist = |store: &Store, row: &LedgerRow, emit: &mut dyn FnMut(&str)| -> bool {
        match store.update_lead_row(row) {
            Ok(()) => true,
            Err(err) => {
                emit(&format!(
                    "[LEADS] 账本写回失败（{}），该行状态未持久化：{err}",
                    row.dedupe_key
                ));
                false
            }
        }
    };
    for row in rows.iter_mut() {
        if row.status != LeadStatus::Approved {
            continue;
        }
        // 无适配器映射的类型（room/未知）静默跳过：不耗预算、不翻面、保持
        // approved 待命——适配器就位之日自然可消费，不为此单设状态（Deferred 已删）。
        if !["search", "creator", "video"].contains(&row.lead_type.as_str()) {
            continue;
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
                if persist(store, row, emit) {
                    consumed += 1;
                }
            }
            Err(err) => {
                row.resolution_note = err
                    .chars()
                    .take(crate::leads::RESOLUTION_NOTE_CAP)
                    .collect();
                persist(store, row, emit);
            }
        }
    }
    consumed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leads::{self, LeadStatus, LedgerRow};

    fn mem_store() -> Store {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        store
            .begin_run_fixed("run:a", "2026-08-05T00:00:00+00:00", "m")
            .unwrap();
        store
    }

    fn seed(store: &Store, rows: &[LedgerRow]) {
        let refs: Vec<&LedgerRow> = rows.iter().collect();
        store.insert_lead_rows(&refs, false).unwrap();
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

    /// 预算计次 + 状态落袋 + 持久化：1 consumed + 1 budget 截断 + 1 pending 未触碰。
    #[test]
    fn budget_caps_attempts_and_writes_back() {
        let store = mem_store();
        seed(
            &store,
            &[
                row("k1", "video", LeadStatus::Approved),
                row("k2", "video", LeadStatus::Approved),
                row("k3", "search", LeadStatus::PendingApproval),
            ],
        );
        let n = consume_approved_leads(&store, 1, &mut |_row| Ok(5), &mut |_: &str| {});
        assert_eq!(n, 1);
        let back = leads::read_rows(&store).unwrap();
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
            consume_approved_leads(&store, 5, &mut |_r| Ok(3), &mut |_: &str| {}),
            1
        );
        assert_eq!(leads::read_rows(&store).unwrap()[1].yield_count, 3);
        assert_eq!(
            consume_approved_leads(&store, 5, &mut |_r| Ok(1), &mut |_: &str| {}),
            0
        );
    }

    /// 补钉：失败的尝试同样烧预算槽——两行 approved + 预算 1 + 恒 Err
    /// → fetch 恰被调用 1 次；k1 留痕、k2 未触碰；预算槽不可被「失败免烧」洞穿。
    #[test]
    fn failing_attempt_burns_budget_slot() {
        let store = mem_store();
        seed(
            &store,
            &[
                row("k1", "video", LeadStatus::Approved),
                row("k2", "video", LeadStatus::Approved),
            ],
        );
        let mut calls = 0_usize;
        let n = consume_approved_leads(
            &store,
            1,
            &mut |_r| {
                calls += 1;
                Err("biu".to_string())
            },
            &mut |_: &str| {},
        );
        assert_eq!(calls, 1, "失败也烧槽：尝试恰一次");
        assert_eq!(n, 0);
        let back = leads::read_rows(&store).unwrap();
        assert_eq!(back[0].status, LeadStatus::Approved);
        assert!(back[0].resolution_note.contains("biu"), "k1 留痕");
        assert!(back[1].resolution_note.is_empty(), "k2 未触碰");
    }

    /// 抓取失败 = 烧预算但保持 approved + 留痕，下轮重试。
    #[test]
    fn failure_keeps_approved_with_note() {
        let store = mem_store();
        seed(&store, &[row("k1", "video", LeadStatus::Approved)]);
        let n = consume_approved_leads(
            &store,
            2,
            &mut |_r| Err("biu".repeat(200)),
            &mut |_: &str| {},
        );
        assert_eq!(n, 0);
        let back = leads::read_rows(&store).unwrap();
        assert_eq!(back[0].status, LeadStatus::Approved);
        assert_eq!(
            back[0].resolution_note.chars().count(),
            crate::leads::RESOLUTION_NOTE_CAP,
            "留痕截断"
        );
    }

    /// room/手编坏类型 → deferred 不烧预算。
    #[test]
    fn unmappable_types_burn_no_budget() {
        let store = mem_store();
        seed(
            &store,
            &[
                row("k1", "room", LeadStatus::Approved),
                row("k2", "nonsense", LeadStatus::Approved),
            ],
        );
        let n = consume_approved_leads(&store, 1, &mut |_r| Ok(9), &mut |_: &str| {});
        assert_eq!(n, 0);
        let back = leads::read_rows(&store).unwrap();
        // 无映射类型静默待命：状态原样 approved、账面无注记、预算零耗
        assert_eq!(
            back,
            &[
                row("k1", "room", LeadStatus::Approved),
                row("k2", "nonsense", LeadStatus::Approved)
            ]
        );
    }

    /// 预算 0 = 完全休眠（默认文化），账目原样、库不读不写。
    #[test]
    fn zero_budget_sleeps() {
        let store = mem_store();
        seed(&store, &[row("k1", "video", LeadStatus::Approved)]);
        assert_eq!(
            consume_approved_leads(&store, 0, &mut |_r| Ok(9), &mut |_: &str| {}),
            0
        );
        assert_eq!(
            leads::read_rows(&store).unwrap()[0].status,
            LeadStatus::Approved
        );
    }

    /// 表形态：写回失败 → 响铃 + 该行不计成功
    /// （不谎报 leads_consumed）。确定性触发：预删目标行 → UPDATE 零命中即 Err。
    #[test]
    fn rewrite_failure_rings_and_does_not_count() {
        let store = mem_store();
        seed(&store, &[row("k1", "video", LeadStatus::Approved)]);
        // fetch 成功但落库前目标行消失 → 写回拒绝
        let mut fetch = |row: &LedgerRow| {
            store
                .conn
                .execute(
                    "DELETE FROM discovery_leads WHERE dedupe_key=?",
                    [row.dedupe_key.clone()],
                )
                .unwrap();
            Ok(9)
        };
        let mut rings: Vec<String> = Vec::new();
        let n = consume_approved_leads(&store, 2, &mut fetch, &mut |m: &str| {
            rings.push(m.to_string())
        });
        assert_eq!(n, 0, "写不回就不计成功");
        assert!(
            rings.iter().any(|m| m.contains("写回失败")),
            "rings={rings:?}"
        );
    }

    /// 空白 locator 在任何网络请求前被拒（不会假证 consumed）。
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
        let store = mem_store();
        seed(&store, &[row("k1", "video", LeadStatus::Approved)]);
        consume_approved_leads(&store, 1, &mut |_r| Ok(1), &mut |_: &str| {});
        let line = crate::leads::summary_line(&leads::read_rows(&store).unwrap(), None);
        assert_eq!(
            line,
            "[lead_ledger] pending=0 approved=0 consumed=1 rejected=0 \
             by_type={video: 1} yield_total=1 latest_consumed=[{\"type\":\"video\",\"locator\":\"loc-k1\",\"yield_count\":1}]",
            "{line}"
        );
    }
}
