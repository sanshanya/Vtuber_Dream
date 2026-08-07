import { budgetBlockOf, type RunRecordView } from "../api";
import { fmtCny } from "../format";

/**
 * 预算闸阻断条（展示-only）：run 终局 outcome.budget_block 时渲染——
 * 直陈预估/预算/新鲜熟客比。省钱模式已删（缓存短路自动只重估输入已变者），
 * 出路两条：调 ai.run_budget_cny，或先跑舰长采集让缓存变新（fresh 缩小自然放行）。
 */
export function BudgetBlockCard({ record }: { record: RunRecordView }) {
  const block = budgetBlockOf(record.outcome);
  if (!block) {
    return null;
  }
  return (
    <div className="budget-block" data-testid="budget-block">
      <p className="budget-block-copy">
        预算阻断：本次预估 ≈{fmtCny(block.estimated_cny)}，超预算 {fmtCny(block.budget_cny)}
        （新鲜 {block.fresh_viewers}/{block.total_viewers} 人）。
      </p>
      <p className="muted small">
        提高 ai.run_budget_cny，或先跑「舰长采集」保持缓存新鲜（fresh 缩小后闸自然放行）。
      </p>
    </div>
  );
}
