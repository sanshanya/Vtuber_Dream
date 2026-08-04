import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { api, type ViewerRow } from "../api";
import { useRunTracker } from "../components/RunTracker";
import { fmtTime } from "../format";

/**
 * 观众列表（D3）+ 空池单查引导位（§12 冷启动 / M5-C 签名件）。
 * EmptyPoolHint 是纯组件：uid 串与提交接线由 props 入——vitest 不摸 fetch。
 */
export function EmptyPoolHint(props: {
  uid: string;
  pending: boolean;
  onUidChange: (uid: string) => void;
  onSubmit: () => void;
}) {
  return (
    <div className="notice" data-testid="empty-pool-hint">
      <h3>观众池为空</h3>
      <p>
        全量感知以大航海名单为入口；名单为空时可以<strong>单查指定观众</strong>
        作为冷启动种子（seed_source=manual）。
      </p>
      <div className="toolbar">
        <input
          aria-label="单查观众 uid"
          placeholder="B站观众 uid（如 1003 或 demo-1）"
          value={props.uid}
          onChange={(event) => props.onUidChange(event.target.value)}
        />
        <button disabled={props.pending || props.uid.trim() === ""} onClick={props.onSubmit}>
          {props.pending ? "已提交…" : "单查该观众"}
        </button>
      </div>
    </div>
  );
}

export function Viewers({ roomId }: { roomId: string }) {
  const viewers = useQuery({ queryKey: ["viewers", roomId], queryFn: () => api.viewers(roomId) });
  const tracker = useRunTracker();
  const [singleUid, setSingleUid] = useState("");
  const [singleError, setSingleError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [submittedRun, setSubmittedRun] = useState<string | null>(null);

  async function submitSingle() {
    setSingleError(null);
    setPending(true);
    try {
      const { run_id } = await api.startRun({ kind: "viewer", viewer_uid: singleUid.trim() });
      setSubmittedRun(run_id);
      // ag4-F1/ag5-F3：单查 run 登记进全局 RunTracker——hero 徽标轮询 + 终态全失效
      // 兑现「完成后列表自动刷新」。
      tracker.track(run_id);
    } catch (error) {
      setSingleError(String(error instanceof Error ? error.message : error));
    } finally {
      setPending(false);
    }
  }

  if (viewers.isLoading) {
    return <div className="empty">载入观众列表…</div>;
  }
  if (viewers.isError) {
    // ag5-F5：错别字修正（面面→列表）。
    return <div className="notice">观众列表加载失败：{String(viewers.error instanceof Error ? viewers.error.message : viewers.error)}</div>;
  }
  const rows = viewers.data ?? [];

  return (
    <section className="section">
      <div className="section-title">
        <h2>舰长列表</h2>
      </div>
      {rows.length === 0 ? (
        <EmptyPoolHint
          uid={singleUid}
          pending={pending}
          onUidChange={setSingleUid}
          onSubmit={() => void submitSingle()}
        />
      ) : (
        <ViewerTable rows={rows} />
      )}
      {singleError && <div className="notice">单查提交被拒：{singleError}</div>}
      {submittedRun && (
        <p className="muted small">
          已触发单查 run：<code>{submittedRun}</code>
          ——完成后列表自动刷新；进度与 events 流见页面顶部页头徽标。
        </p>
      )}
    </section>
  );
}

function ViewerTable({ rows }: { rows: ViewerRow[] }) {
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            <th>观众</th>
            <th>uid</th>
            <th>采集于</th>
            <th>Perception</th>
            <th>入口</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.uid}>
              <td>{row.name ?? "—"}</td>
              <td className="protocol">{row.uid}</td>
              <td>{fmtTime(row.collected_at)}</td>
              <td>
                <span className={`badge ${row.ai_completed ? "state" : ""}`}>
                  {row.ai_status ?? "未运行"}
                </span>
              </td>
              <td>
                <a href={`#/viewers/${encodeURIComponent(row.uid)}/tree`}>个人树</a> ·{" "}
                <a href={`#/viewers/${encodeURIComponent(row.uid)}/graph`}>局部图</a>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
