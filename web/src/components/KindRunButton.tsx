import { useState } from "react";

import { activeRunIdFrom, api, RUN_KIND_LABELS, type RunKind } from "../api";
import { useRunTracker } from "./RunTracker";

/** Z4 动作平面：四个分层 kind（full 走谨慎 RunButton；viewer 走舰长表单）。 */
type StagedKind = Exclude<RunKind, "full" | "viewer">;

interface RunMessage {
  tone: "danger" | "muted";
  text: string;
}

/**
 * Z4 动作平面分层钮：采集（事实层）/AI（认知层）分 kind，一击即飞、就地摆位——
 * 哪个页面的数据由哪个动作产出，钮就住在哪个页面。
 * - 成功：登记 RunTracker（页头徽标接管进度回报，终态全失效自动刷新本页）；
 * - 409：错文里的在飞 run_id 提取后改为跟随（与后端单飞互斥契约同构）；
 * - 其他错误（422 参数面、5xx）：就地 danger 徽标。
 */
export function KindRunButton({ kind, note }: { kind: StagedKind; note?: string }) {
  const tracker = useRunTracker();
  const [message, setMessage] = useState<RunMessage | null>(null);

  async function trigger() {
    setMessage(null);
    try {
      const { run_id } = await api.startRun({ kind });
      tracker.track(run_id);
      setMessage({
        tone: "muted",
        text: `已提交 ${run_id.slice(0, 8)}…进度见页头徽标，完成后本页自动刷新`,
      });
    } catch (error) {
      const text = String(error instanceof Error ? error.message : error);
      const active = activeRunIdFrom(text);
      if (active) {
        tracker.track(active);
        setMessage({ tone: "muted", text: "已有进行中的 run——页头徽标转为跟随其进度" });
      } else {
        setMessage({ tone: "danger", text });
      }
    }
  }

  return (
    <span className="kind-run" data-testid={`kind-run-${kind}`}>
      <button className="secondary" disabled={tracker.active} onClick={() => void trigger()}>
        {tracker.active ? "运行中…" : RUN_KIND_LABELS[kind]}
      </button>
      {message && (
        <span className={message.tone === "danger" ? "badge danger" : "muted small"}>
          {message.text}
        </span>
      )}
      {note && <span className="muted small">{note}</span>}
    </span>
  );
}
