import { useState } from "react";

import { useStartRun } from "../hooks/useStartRun";
import { isRunActive, useRunTracker } from "./RunTracker";

/**
 * 单查触发钮（ag5-F7：ViewerTree 空轴引导位）。提交成功 → run_id 登记进
 * 全局 RunTracker（终态后各页自动失效刷新）；进度不再本组件自显——hero 徽标承担。
 */
export function SingleViewerRunButton({ vid, label }: { vid: string; label?: string }) {
  const tracker = useRunTracker();
  const { start, error, clearError } = useStartRun();
  const [pending, setPending] = useState(false);
  const [submitted, setSubmitted] = useState<string | null>(null);

  async function trigger() {
    clearError();
    setPending(true);
    try {
      // R3-F1：viewer 属单飞互斥契约——409 错文在飞 id 由 useStartRun 转 RunTracker
      // 跟随（与 KindRunButton 同构）并照返在飞 id：入口翻转成跟随态，不裸报错。
      const runId = await start({ kind: "viewer", viewer_uid: vid });
      if (runId !== null) setSubmitted(runId);
    } finally {
      setPending(false);
    }
  }

  // R3#4：submitted 不是永久锁——tracker 见我那条 run 的终态帧后释放，入口恢复可再发
  //（丢失/被别口接管时不擅自放行：在飞与否以 tracker 可见帧为准）。
  const terminalSeen =
    submitted !== null &&
    tracker.runId === submitted &&
    tracker.record !== undefined &&
    !isRunActive(tracker.record);
  const locked = submitted !== null && !terminalSeen;

  return (
    <span className="run-trigger" data-testid={`single-run-${vid}`}>
      <button disabled={pending || tracker.active || locked} onClick={() => void trigger()}>
        {locked ? "已提交（进度见页头徽标）" : (label ?? "触发该观众单查")}
      </button>
      {error && <span className="badge danger">{error}</span>}
    </span>
  );
}
