import { useState } from "react";

import { budgetBlockOf, type RunKind, type RunRecordView } from "../api";
import { fmtCny } from "../format";
import { useStartRun, type StartRunBody } from "../hooks/useStartRun";

/** spend_mode 重发只对带单人感知段的 kind 合法（= /api/runs 校验同源）；
 *  ai_audience 的阻断是平段超支，两发射极对它是 422，不摆钮只摆链路。 */
export const SPEND_MODE_KINDS: ReadonlyArray<RunKind> = ["full", "viewer", "ai_viewers"];

/**
 * 阻断卡（spend_mode 前端行动面）：run 终局 outcome.budget_block 时渲染——
 * 直陈预估/预算/新鲜熟客比；两选重发（只跑增量 / 只推简报，spend_mode 随行、
 * 保留原 kind/viewer_uid）；或链接去设置页改预算。hint 文案服务端 verbatim 透传。
 */
export function BudgetBlockCard({ record }: { record: RunRecordView }) {
  const block = budgetBlockOf(record.outcome);
  const { start, error, clearError } = useStartRun();
  const [submitted, setSubmitted] = useState<string | null>(null);
  if (!block) {
    return null;
  }

  async function resend(spend_mode: "incremental" | "briefing_only") {
    clearError();
    setSubmitted(null);
    const body: StartRunBody = { kind: record.kind as RunKind, spend_mode };
    if (record.viewer_uid) {
      body.viewer_uid = record.viewer_uid;
    }
    const runId = await start(body);
    if (runId !== null) setSubmitted(runId);
  }

  const canResend = SPEND_MODE_KINDS.includes(record.kind as RunKind);

  return (
    <div className="budget-block" data-testid="budget-block">
      <p className="budget-block-copy">
        预算阻断：本次预估 ≈{fmtCny(block.estimated_cny)}，超预算 {fmtCny(block.budget_cny)}
        （新鲜 {block.fresh_viewers}/{block.total_viewers} 人）。
      </p>
      <p className="muted small">{block.hint}</p>
      {canResend && (
        <span className="budget-block-actions">
          <button data-testid="budget-retry-incremental" onClick={() => void resend("incremental")}>
            只跑增量
          </button>
          <button data-testid="budget-retry-briefing" onClick={() => void resend("briefing_only")}>
            只推简报
          </button>
        </span>
      )}
      {submitted && (
        <span className="muted small">已重发 {submitted.slice(0, 8)}…（进度见页头徽标）</span>
      )}
      {error && <span className="badge danger">{error}</span>}
      <a className="budget-block-link" href="#/settings">
        去设置页改预算
      </a>
    </div>
  );
}