import { useState } from "react";

import { type RunRecordView } from "../api";
import { RUN_EVENTS_CAP } from "../constants";
import { fmtTime } from "../format";
import { useStartRun } from "../hooks/useStartRun";
import { BudgetBlockCard } from "./BudgetBlockCard";
import { useRunTracker } from "./RunTracker";

/** outcome.error 摘取（failed 终局体的唯一用户可读位；非对象 outcome → null）。 */
function outcomeErrorOf(outcome: unknown): string | null {
  if (outcome && typeof outcome === "object" && "error" in outcome) {
    return String((outcome as { error: unknown }).error);
  }
  return null;
}

/** partial 徽标的解释走 title（服务端不重复造词，前端一次性说清语义）。 */
export function partialTitle(record: RunRecordView): string {
  const base = `触发于 ${fmtTime(record.started_at)}`;
  return record.partial
    ? `${base} · partial = 完成但仍保留观众级失败（详见 events 与 leads）`
    : base;
}

/**
 * run 状态面从触发钮拆出——hero 页头挂只读徽标，各页面各自摆触发钮。
 * 数据源仍是 RunTracker 共享层：任何页面触发的 run 都在此处反映（单槽与后端
 * 409 互斥同构，任何时刻至多一条在飞）。
 */
export function RunStatusBadge() {
  const tracker = useRunTracker();
  const data = tracker.record;
  const events = (data?.events ?? []).slice(-RUN_EVENTS_CAP);
  const outcomeError = outcomeErrorOf(data?.outcome);
  if (!data && !tracker.lost) {
    return null;
  }
  return (
    <span className="run-status" data-testid="run-status">
      {data && (
        <span className={`badge run-status-${data.status}`} title={partialTitle(data)}>
          {data.status}
          {data.partial ? "(partial)" : ""}
          {/* demo 快照与真实 done 逐像素同形是合成诡装真实——明示合成。 */}
          {data.kind === "demo" ? "（synthetic_demo 合成演示）" : ""}
        </span>
      )}
      {tracker.lost && (
        // dismiss 不能靠悬停 tooltip 藏——可见 × 是丢链的唯一显式出路。
        <button
          className="badge danger"
          title="点击消除提示"
          onClick={() => tracker.dismissLost()}
        >
          {tracker.lost} ×
        </button>
      )}
      {outcomeError && <span className="badge danger">{outcomeError}</span>}
      {/* events 不再只在 active 时渲染——终态恰是最需要排查的时刻。 */}
      {data && events.length > 0 && (
        <details className="run-events">
          <summary>events ({events.length})</summary>
          <pre>{events.join("\n")}</pre>
        </details>
      )}
      {/* outcome.budget_block 阻断卡（两选重发 + 去设置页改预算）。 */}
      {data && <BudgetBlockCard record={data} />}
    </span>
  );
}

/**
 * 全量感知 = 「敏感而谨慎」的钮（用户定调）——不在 hero 页头常驻，落在主播
 * 介绍页动作栏，一击不飞：先展开成本/时长确认段，再击「确认触发」才提交。
 * 其余四个分层动作（采集/AI × 主播/舰长）走 KindRunButton。
 */
export function RunButton({ viewerCount }: { viewerCount?: number | null }) {
  const tracker = useRunTracker();
  const { start, error: submitError, clearError } = useStartRun();
  const [armed, setArmed] = useState(false);

  async function trigger() {
    setArmed(false);
    clearError();
    await start({ kind: "full" });
  }

  return (
    <span className="run-trigger">
      {armed ? (
        <span className="run-confirm" data-testid="full-run-confirm">
          <p className="run-confirm-copy">
            全量感知 = 采集 + AI 先后连环进：重抓主播档案、大航海名单与每人近态（事实层），
            再逐舰长 AI + 整体态势（认知层）。实测 5 名舰长 ≈54 分钟、估算 ≈¥6.6
            （DeepSeek 价目上界；规模随舰长数伸缩
            {typeof viewerCount === "number" ? `，当前名单 ${viewerCount} 人` : ""}）。
            重采只重建事实采集面，旧 AI 结论保留作参考——事实有变的舰长会被亮
            「信源已更新·待重判」，本轮只重判他们（哈希失效驱动，未变的零成本复用）。
            运行期间一切触发 409 互斥。
          </p>
          <span className="run-confirm-actions">
            <button className="primary" onClick={() => void trigger()}>
              确认触发全量感知
            </button>
            <button onClick={() => setArmed(false)}>取消</button>
          </span>
        </span>
      ) : (
        <button
          className="primary"
          disabled={tracker.active}
          onClick={() => setArmed(true)}
          title="敏感动作：采集 + AI 连环，先展开成本确认"
        >
          {tracker.active ? "运行中…" : "触发全量感知"}
        </button>
      )}
      {submitError && <span className="badge danger">{submitError}</span>}
      <RunStatusBadge />
    </span>
  );
}
