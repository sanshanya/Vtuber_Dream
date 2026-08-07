//! 「花费预算闸」核心件。证据锚 docs/2026-08-06-r2-roster-budget-evidence.md
//! （22 舰长全量 ¥34.49、≈¥1.5/人、audience ≈¥1.5、基线 ≤¥3/轮被爆 11 倍）；口径 =
//! web/src/format.ts estimateCostCny `(input×2+output×8)/1_000_000` CNY。

use std::path::Path;

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

/// 省钱模式（三选收窄两选：b「只跑分层」随名册分档冻结缺席）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpendMode {
    /// 全量：名册全员扇出（默认，现状一字不动）。
    #[default]
    Normal,
    /// 只更新「有完整旧结论且输入已变」的人——纯新建者本轮缺席。
    IncrementalOnly,
    /// 跳过单人感知、只推简报（audience 段照跑）。
    BriefingOnly,
}

impl SpendMode {
    /// 只收字面 `incremental` / `briefing_only`；其余一律报错（Normal 不经此处）。
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "incremental" => Ok(Self::IncrementalOnly),
            "briefing_only" => Ok(Self::BriefingOnly),
            _ => Err("spend_mode 只收 incremental / briefing_only".to_string()),
        }
    }

    /// 与 parse 相对的字面面（Normal 也回来）——progress/budget.json 用此串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::IncrementalOnly => "incremental",
            Self::BriefingOnly => "briefing_only",
        }
    }
}

impl std::fmt::Display for SpendMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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

/// `{output_dir}/ai/budget.json` 落盘（放行/阻断都写；写失败由调用方响铃不绊管线。新侧文件零 parity 风险）。
#[allow(clippy::too_many_arguments)]
pub fn write_budget_file(
    output_dir: &Path,
    spend_mode: SpendMode,
    estimated_cny: f64,
    budget_cny: Option<f64>,
    fresh_viewers: usize,
    total_viewers: usize,
    gate: &str,
    at: &str,
) -> Result<(), String> {
    crate::storage::write_json(
        &output_dir.join("ai").join("budget.json"),
        &serde_json::json!({
            "spend_mode": spend_mode.as_str(),
            "estimated_cny": estimated_cny,
            "budget_cny": budget_cny,
            "fresh_viewers": fresh_viewers,
            "total_viewers": total_viewers,
            "gate": gate,
            "at": at,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-08-06T00:00:00.000000+00:00";

    fn read(d: &Path) -> serde_json::Value {
        let p = d.join("ai").join("budget.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

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
    fn briefing_flat_when_no_fresh_viewers() {
        assert!((decide_budget(0, true, None).estimated_cny - 1.5).abs() < 1e-9);
        assert_eq!(decide_budget(0, false, None).estimated_cny, 0.0);
    }

    #[test]
    fn spend_mode_parse_and_str_roundtrip() {
        let inc = SpendMode::parse("incremental").unwrap();
        assert_eq!(inc, SpendMode::IncrementalOnly);
        let brief = SpendMode::parse("briefing_only").unwrap();
        assert_eq!(brief, SpendMode::BriefingOnly);
        for bad in ["normal", "正常", "分层", ""] {
            let err = SpendMode::parse(bad).unwrap_err();
            assert!(err.contains("incremental"), "{bad}: {err}");
            assert!(err.contains("briefing_only"), "{bad}: {err}");
        }
        assert_eq!(SpendMode::Normal.as_str(), "normal");
        assert_eq!(format!("{}", SpendMode::BriefingOnly), "briefing_only");
        assert_eq!(SpendMode::default(), SpendMode::Normal);
    }

    #[test]
    fn budget_file_key_set_and_gate_literal() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write_budget_file(d, SpendMode::Normal, 3.5, Some(3.0), 1, 22, "blocked", AT).unwrap();
        assert_eq!(
            read(d),
            serde_json::json!({
                "spend_mode": "normal", "estimated_cny": 3.5, "budget_cny": 3.0,
                "fresh_viewers": 1, "total_viewers": 22, "gate": "blocked", "at": AT,
            })
        );
    }

    #[test]
    fn budget_file_null_budget_and_proceed_gate() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write_budget_file(d, SpendMode::BriefingOnly, 1.5, None, 0, 22, "proceed", AT).unwrap();
        assert_eq!(
            read(d),
            serde_json::json!({
                "spend_mode": "briefing_only", "estimated_cny": 1.5, "budget_cny": null,
                "fresh_viewers": 0, "total_viewers": 22, "gate": "proceed", "at": AT,
            })
        );
    }
}
