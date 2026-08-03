import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { api, type ViewerRow } from "../api";
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
          placeholder="B站观众 uid（数字）"
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
      // 单查到达终态时的全量刷新由页头 RunButton 轮询链承担，这里只标已发。
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
    return <div className="notice">观众面面加载失败：{String(viewers.error instanceof Error ? viewers.error.message : viewers.error)}</div>;
  }
  const rows = viewers.data ?? [];

  return (
    <section className="section">
      <div className="section-title">
        <h2>观众列表</h2>
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
          ——回到 <a href="#/">房间面板</a> 看 events 流。
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
