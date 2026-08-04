import { useState } from "react";

import { api, type RunRecordView } from "../api";
import { RUN_EVENTS_CAP } from "../constants";
import { fmtTime } from "../format";
import { useRunTracker } from "./RunTracker";

/** outcome.error 摘取（failed 终局体的唯一用户可读位；非对象 outcome → null）。 */
function outcomeErrorOf(outcome: unknown): string | null {
  if (outcome && typeof outcome === "object" && "error" in outcome) {
    return String((outcome as { error: unknown }).error);
  }
  return null;
}

/** ag5-F8：partial 徽标的解释走 title（服务端不重复造词，前端一次性说清语义）。 */
export function partialTitle(record: RunRecordView): string {
  const base = `触发于 ${fmtTime(record.started_at)}`;
  return record.partial
    ? `${base} · partial = 完成但仍保留观众级失败（详见 events 与 leads）`
    : base;
}

/**
 * hero 单点触发钮（design §10「全部页头」裁决）+ 状态徽标。
 * run 追踪的全部状态位由 RunTracker 共享层供给——本组件不再自持 runId（ag4-F1）。
 */
export function RunButton() {
  const tracker = useRunTracker();
  const [submitError, setSubmitError] = useState<string | null>(null);

  async function trigger() {
    setSubmitError(null);
    try {
      const { run_id } = await api.startRun({ kind: "full" });
      tracker.track(run_id);
    } catch (error) {
      setSubmitError(String(error instanceof Error ? error.message : error));
    }
  }

  const data = tracker.record;
  const events = (data?.events ?? []).slice(-RUN_EVENTS_CAP);
  const outcomeError = outcomeErrorOf(data?.outcome);
  return (
    <span className="run-trigger">
      <button className="primary" disabled={tracker.active} onClick={() => void trigger()}>
        {tracker.active ? "运行中…" : "触发全量感知"}
      </button>
      {data && (
        <span className={`badge run-status-${data.status}`} title={partialTitle(data)}>
          {data.status}
          {data.partial ? "(partial)" : ""}
          {/* W2/r1-F3：demo 快照与真实 done 逐像素同形是合成诡装真实——明示合成。 */}
          {data.kind === "demo" ? "（synthetic_demo 合成演示）" : ""}
        </span>
      )}
      {tracker.lost && (
        // W2/r5-F2：dismiss 不能靠悬停 tooltip 藏——可见 × 是丢链的唯一显式出路。
        <button
          className="badge danger"
          title="点击消除提示"
          onClick={() => tracker.dismissLost()}
        >
          {tracker.lost} ×
        </button>
      )}
      {outcomeError && <span className="badge danger">{outcomeError}</span>}
      {submitError && <span className="badge danger">{submitError}</span>}
      {/* ag5-F2：events 不再只在 active 时渲染——终态恰是最需要排查的时刻。 */}
      {data && events.length > 0 && (
        <details className="run-events">
          <summary>events ({events.length})</summary>
          <pre>{events.join("\n")}</pre>
        </details>
      )}
    </span>
  );
}
