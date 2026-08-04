import { useState } from "react";

import { activeRunIdFrom, api } from "../api";
import { useRunTracker } from "./RunTracker";

/**
 * 单查触发钮（ag5-F7：ViewerTree 空轴引导位）。提交成功 → run_id 登记进
 * 全局 RunTracker（终态后各页自动失效刷新）；进度不再本组件自显——hero 徽标承担。
 */
export function SingleViewerRunButton({ vid, label }: { vid: string; label?: string }) {
  const tracker = useRunTracker();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState<string | null>(null);

  async function trigger() {
    setError(null);
    setPending(true);
    try {
      const { run_id } = await api.startRun({ kind: "viewer", viewer_uid: vid });
      setSubmitted(run_id);
      tracker.track(run_id);
    } catch (thrown) {
      const text = String(thrown instanceof Error ? thrown.message : thrown);
      // R3-F1：viewer 属单飞互斥契约——409 错文在飞 id 转 RunTracker 跟随
      //（与 KindRunButton 同构）：入口翻转成跟随态，不裸报错。
      const active = activeRunIdFrom(text);
      if (active) {
        tracker.track(active);
        setSubmitted(active);
      } else {
        setError(text);
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <span className="run-trigger" data-testid={`single-run-${vid}`}>
      <button disabled={pending || tracker.active || submitted !== null} onClick={() => void trigger()}>
        {submitted !== null ? "已提交（进度见页头徽标）" : (label ?? "触发该观众单查")}
      </button>
      {error && <span className="badge danger">{error}</span>}
    </span>
  );
}
