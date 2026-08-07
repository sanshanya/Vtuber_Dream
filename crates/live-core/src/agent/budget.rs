//! 「花费预算闸」核心件。证据锚 docs/2026-08-06-r2-roster-budget-evidence.md
//! （22 舰长全量 ¥34.49、≈¥1.5/人、audience ≈¥1.5、基线 ≤¥3/轮被爆 11 倍）；口径 =
//! web/src/format.ts estimateCostCny `(input×2+output×8)/1_000_000` CNY。
//!
//! 删码刀3 收口：SpendMode/账本落盘全删——预估口径与执行语义同源（fresh = 输入哈希
//! 已变 ∪ 无完整旧结论，由 pipeline::fresh_viewer_ids 唯一产出），只剩估算公式 +
//! 硬闸判定两个纯函数。实耗不另设账本：usage 本来就在 ai/state.json。

/// 入/出 token 费率（¥/百万 token；web TOKEN_RATES_CNY_PER_MILLION 同值）。
pub const TOKEN_RATE_INPUT_CNY_PER_MILLION: f64 = 2.0;
pub const TOKEN_RATE_OUTPUT_CNY_PER_MILLION: f64 = 8.0;
/// 单观众预估 tokens（audience 同费率平摊）；人均该段合计恰 ¥1.5 = 证据锚 22 人 ¥34.49 的对账件。
pub const PER_VIEWER_EST_INPUT_TOKENS: i64 = 500_000;
pub const PER_VIEWER_EST_OUTPUT_TOKENS: i64 = 62_500;
pub const AUDIENCE_EST_INPUT_TOKENS: i64 = 500_000;
pub const AUDIENCE_EST_OUTPUT_TOKENS: i64 = 62_500;

/// 与 web/src/format.ts estimateCostCny 同公式（两段式求和 ÷ 1_000_000）。
pub fn cost_cny(input_tokens: i64, output_tokens: i64) -> f64 {
    (input_tokens as f64 * TOKEN_RATE_INPUT_CNY_PER_MILLION
        + output_tokens as f64 * TOKEN_RATE_OUTPUT_CNY_PER_MILLION)
        / 1_000_000.0
}

/// 预算闸判定结果（预估/预算原值 + 放行·阻断二态）。
pub struct BudgetCheck {
    pub estimated_cny: f64,
    pub budget_cny: Option<f64>,
    pub blocked: bool,
}

/// 预估一轮 run 花费：fresh 人均 × 人数 + audience 整体段（选项开关，flat 一刀）。
pub fn estimate_run_cost_cny(fresh_viewers: usize, include_audience: bool) -> f64 {
    let per_viewer = cost_cny(PER_VIEWER_EST_INPUT_TOKENS, PER_VIEWER_EST_OUTPUT_TOKENS);
    let audience_flat = cost_cny(AUDIENCE_EST_INPUT_TOKENS, AUDIENCE_EST_OUTPUT_TOKENS);
    fresh_viewers as f64 * per_viewer + if include_audience { audience_flat } else { 0.0 }
}

/// 放行/阻断：预算存在且预估**严格大于**预算 → blocked（等于放行，不误伤临界）；
/// budget=None 永不阻断（不设闸 = 现状一字不动）。
pub fn decide_budget(
    fresh_viewers: usize,
    include_audience: bool,
    budget_cny: Option<f64>,
) -> BudgetCheck {
    let estimated_cny = estimate_run_cost_cny(fresh_viewers, include_audience);
    let blocked = budget_cny.is_some_and(|budget| estimated_cny > budget);
    BudgetCheck {
        estimated_cny,
        budget_cny,
        blocked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_cny_formula_anchor() {
        assert!((cost_cny(1_000_000, 1_000_000) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn constant_anchor_22_viewers_plus_audience() {
        let check = decide_budget(22, true, None);
        assert!((check.estimated_cny - 34.5).abs() < 1e-9, "预估应≈34.5");
        assert!(!check.blocked, "budget=None 永不阻断");
    }

    #[test]
    fn boundary_equal_passes_strict_greater_blocks() {
        let normal = decide_budget(2, false, Some(3.0));
        assert!(!normal.blocked, "预估==预算 放行");
        let over = decide_budget(2, false, Some(3.0 - 0.0001));
        assert!(over.blocked, "预估>预算 阻断");
    }

    #[test]
    fn empty_fresh_is_audience_flat_only() {
        assert!((decide_budget(0, true, None).estimated_cny - 1.5).abs() < 1e-9);
        assert_eq!(decide_budget(0, false, None).estimated_cny, 0.0);
    }
}
