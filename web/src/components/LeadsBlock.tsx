/**
 * leads 区块（公示面 + 审批缝 + 拒绝面）。
 * 四态计数徽标 + pending 明细直出；待审行带「批准/拒绝」钮（一击即飞；
 * data-testid=lead-approve-{dedupe_key} / lead-reject-{dedupe_key}）——
 * 动作本身由宿主页（Leads）经 POST 审批/拒绝缝承担，本块只做呈现与击发。
 *
 * pending 按持有人（viewer_id）分组、组默认折叠（<details> 闭合态其内容仍
 * 在 DOM 中——查询与一击操作不依赖视觉展开）；组头带「全批」组级钮
 * （前端逐行 fan-out，不造批量服务端面）。拒因面 = 行内可展开单 reason
 * 自由文本 ≤80 字（空也合法：服务端 NULL 留档）；拒绝钮提交时取该行当前文本。
 * rejected 明细直出（leads.rejected）：徽标即 <details>，展开逐行回看记录
 * 的 note——只读事实面，绝不代行裁决。
 *
 * leads.summary 的 [lead_ledger] 裸文本不再上墙；空账显式空态一句；
 * 待审行 key = dedupe_key（非索引）；在飞期间钮禁且 onClick 自身再做 busy 护栏。
 */
import { useState } from "react";

export interface LeadRow {
  dedupe_key?: string;
  /** 服务端 serde rename 出的 wire 键（leads.rs LedgerRow）。 */
  type?: string;
  locator?: string;
  viewer_id?: string;
  motivation?: string;
  priority?: string;
  /** 拒绝留档面（rejected 明细直出；pending 行恒空）。 */
  reject_note?: string;
}

export interface LeadsView {
  totals?: {
    pending_approval?: number;
    approved?: number;
    consumed?: number;
    rejected?: number;
  };
  /** 账本直出行。 */
  pending?: LeadRow[];
  /** rejected 明细面（服务端 overview 直出；拒因留档可回看）。 */
  rejected?: LeadRow[];
}

const TOTAL_LABELS: Array<[keyof NonNullable<LeadsView["totals"]>, string]> = [
  ["pending_approval", "待审批"],
  ["approved", "已批准"],
  ["consumed", "已消费"],
  ["rejected", "已拒绝"],
];

export interface LeadsBlockProps {
  leads: LeadsView;
  /** 批准回调（宿主页持有 mutation）；缺省 = 纯读面不渲染钮。 */
  onApprove?: (leadId: string) => void;
  /** 拒绝回调；第二参 = 行内当前拒因文本（空合法）。 */
  onReject?: (leadId: string, reason: string) => void;
  /** 在飞批准的目标 id（一击即飞的禁用面）。 */
  busyLeadId?: string | null;
  /** 在飞拒绝的目标 id。 */
  busyRejectId?: string | null;
}

export function LeadsBlock({
  leads,
  onApprove,
  onReject,
  busyLeadId,
  busyRejectId,
}: LeadsBlockProps) {
  const totals = leads.totals ?? {};
  const pending = leads.pending ?? [];
  const rejected = leads.rejected ?? [];
  // 行内拒因编辑态：key = dedupe_key；写了还没击「拒绝」不算动作。
  const [reasonNote, setReasonNote] = useState<Record<string, string>>({});
  // 空账 = 五态全零（summary 裸账本行不再上墙，人话面只留五态徽标）。
  const ledgerEmpty = TOTAL_LABELS.every(([key]) => (totals[key] ?? 0) === 0);
  // pending 按持有人（viewer_id）分组；无主行收进「待归主」组——插入序保序。
  const groups = pending.reduce<Map<string, LeadRow[]>>((acc, row) => {
    const holder = row.viewer_id ?? "待归主";
    const bucket = acc.get(holder) ?? [];
    bucket.push(row);
    acc.set(holder, bucket);
    return acc;
  }, new Map());
  // 组级钮：组内任一行在飞则整组按住（双击/连击不得撕开口子）。
  const groupBusy = busyLeadId != null || busyRejectId != null;
  return (
    <div>
      <div className="badges" data-testid="leads-totals">
        {TOTAL_LABELS.map(([key, label]) => {
          const value = totals[key] ?? 0;
          if (key === "rejected" && rejected.length > 0) {
            // badge 即 <details>——展开逐行回看记录拒因（只读事实面）。
            return (
              <details key={key} className="badge" data-testid="leads-rejected">
                <summary>
                  {label} {value}
                </summary>
                <ul className="rejected-list">
                  {rejected.map((row, i) => {
                    const note = (row.reject_note ?? "").trim();
                    return (
                      <li key={row.dedupe_key ?? `rejected-${i}`}>
                        <span className="badge fact">{String(row.type ?? "?")}</span>{" "}
                        {String(row.locator ?? "?")}
                        {note && <span className="muted"> · “{note}”</span>}
                      </li>
                    );
                  })}
                </ul>
              </details>
            );
          }
          return (
            <span className="badge" key={key}>
              {label} {value}
            </span>
          );
        })}
      </div>
      {ledgerEmpty && (
        <p className="empty" data-testid="leads-empty">
          暂无线索——主播 AI 分析或全量感知跑过后，可疑方向会落进这里。
        </p>
      )}
      {groups.size > 0 && (
        <div className="lead-groups">
          {[...groups.entries()].map(([holder, rows]) => {
            const batchApprove = () =>
              rows.forEach((row) => {
                if (row.dedupe_key != null && onApprove) onApprove(String(row.dedupe_key));
              });
            return (
              <details key={holder} className="lead-group" data-testid={`lead-group-${holder}`}>
                <summary className="lead-group-summary">
                  {holder} · {rows.length} 条
                  {onApprove ? (
                    <button
                      className="badge"
                      data-testid={`lead-approve-all-${holder}`}
                      disabled={groupBusy}
                      onClick={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        batchApprove();
                      }}
                    >
                      全批
                    </button>
                  ) : null}
                </summary>
                <ul className="delta-list">
                  {rows.map((row, i) => {
                    // leads 系工具产物快照——入 JSX 的键一律 String() 护栏。
                    const leadId = row.dedupe_key == null ? null : String(row.dedupe_key);
                    const note = leadId ? (reasonNote[leadId] ?? "") : "";
                    return (
                      // 列表 key = dedupe_key（身份随数据，不随索引漂移）。
                      <li key={leadId ?? `no-key-${i}`}>
                        <span className="badge fact">{String(row.type ?? "?")}</span>{" "}
                        {String(row.locator ?? "?")}
                        {row.viewer_id ? <span className="muted"> @{String(row.viewer_id)}</span> : null}
                        {row.motivation ? <span className="muted"> · {String(row.motivation)}</span> : null}{" "}
                        {onReject && leadId ? (
                          <button
                            className="badge"
                            data-testid={`lead-reject-${leadId}`}
                            disabled={busyRejectId === leadId}
                            onClick={() => {
                              // 一击即飞、无 dialog；提交行内当前拒因文本（空合法）。
                              if (busyRejectId !== leadId) onReject(leadId, note);
                            }}
                          >
                            {busyRejectId === leadId ? "拒绝中…" : "拒绝"}
                          </button>
                        ) : null}
                        {onApprove && leadId ? (
                          <button
                            className="badge"
                            data-testid={`lead-approve-${leadId}`}
                            disabled={busyLeadId === leadId}
                            onClick={() => {
                              // 在飞护栏：disabled 之外的第二道闸——双击/连击只发一次 POST。
                              if (busyLeadId !== leadId) onApprove(leadId);
                            }}
                          >
                            {busyLeadId === leadId ? "批准中…" : "批准"}
                          </button>
                        ) : null}
                        {leadId && onReject ? (
                          <details className="lead-reasons" data-testid={`lead-reasons-${leadId}`}>
                            <summary>拒因…</summary>
                            <div className="lead-reasons-body">
                              <input
                                className="lead-note"
                                data-testid={`lead-reject-note-${leadId}`}
                                value={note}
                                maxLength={80}
                                disabled={busyRejectId === leadId}
                                placeholder="拒因（可选，≤80 字，拒绝时随行）"
                                onChange={(e) =>
                                  setReasonNote((prev) => ({
                                    ...prev,
                                    [leadId]: e.target.value,
                                  }))
                                }
                              />
                            </div>
                          </details>
                        ) : null}
                      </li>
                    );
                  })}
                </ul>
              </details>
            );
          })}
        </div>
      )}
    </div>
  );
}