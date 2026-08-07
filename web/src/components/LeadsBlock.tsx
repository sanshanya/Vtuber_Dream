/**
 * leads 区块（M4.x 公示面 + G2-B 审批缝 + 拒绝面）。
 * 五态计数徽标 + pending 明细直出；待审行带「批准/拒绝」钮（一击即飞；
 * data-testid=lead-approve-{dedupe_key} / lead-reject-{dedupe_key}）——
 * 动作本身由宿主页（Leads）经 POST 审批/拒绝缝承担，本块只做呈现与击发。
 *
 * pending 按持有人（viewer_id）分组、组默认折叠（<details> 闭合态其内容仍
 * 在 DOM 中——查询与一击操作不依赖视觉展开）；组头带「全批/全拒」组级钮
 * （前端逐行 fan-out，不造批量服务端面）。拒因面 = 行内可展开区（chip 白名单
 * = overview `leads.reject_chip_reasons` 直出——服务端唯一真源，前端不落第二份
 * 字面 + 一条自由注记 ≤80 字，
 * 全空也合法：服务端 NULL/NULL 留档）；拒绝钮提交时取该行当前已选拒因。
 * rejected 明细直出（leads.rejected）：徽标即 <details>，展开逐行回看记录
 * 的 chip/note——只读事实面，绝不代行裁决。
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
  reject_chips?: string[];
  reject_note?: string;
}

export interface LeadsView {
  totals?: {
    pending_approval?: number;
    approved?: number;
    consumed?: number;
    rejected?: number;
    deferred?: number;
  };
  /** G2-B：自治位读取面（0=纯人工 / 1=L1 自动批准+预算消费）。 */
  autonomy?: number;
  /** 账本直出行。 */
  pending?: LeadRow[];
  /** rejected 明细面（服务端 overview 直出；拒因留档可回看）。 */
  rejected?: LeadRow[];
  /** 拒因 chip 白名单（服务端 overview 直出 = live-core REJECT_CHIP_REASONS；
   *  唯一真源——缺席旧端回落空面，不在前端落第二份字面）。 */
  reject_chip_reasons?: string[];
}

const TOTAL_LABELS: Array<[keyof NonNullable<LeadsView["totals"]>, string]> = [
  ["pending_approval", "待审批"],
  ["approved", "已批准"],
  ["consumed", "已消费"],
  ["rejected", "已拒绝"],
  ["deferred", "暂缓"],
];

export interface LeadsBlockProps {
  leads: LeadsView;
  /** 批准回调（宿主页持有 mutation）；缺省 = 纯读面不渲染钮。 */
  onApprove?: (leadId: string) => void;
  /** 拒绝回调；第三参 = 行内当前已选拒因（全空合法）。 */
  onReject?: (leadId: string, chips: string[], note: string) => void;
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
  // 行内拒因编辑态：key = dedupe_key；手里选了 chip 但还没击「拒绝」不算动作。
  const [reasonChips, setReasonChips] = useState<Record<string, string[]>>({});
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
  const toggleChip = (leadId: string, chip: string) => {
    setReasonChips((prev) => {
      const cur = prev[leadId] ?? [];
      return {
        ...prev,
        [leadId]: cur.includes(chip) ? cur.filter((c) => c !== chip) : [...cur, chip],
      };
    });
  };
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
                    const chips = row.reject_chips ?? [];
                    const note = (row.reject_note ?? "").trim();
                    return (
                      <li key={row.dedupe_key ?? `rejected-${i}`}>
                        <span className="badge fact">{String(row.type ?? "?")}</span>{" "}
                        {String(row.locator ?? "?")}
                        {chips.length > 0 && (
                          <span className="muted"> · 拒因：{chips.join("、")}</span>
                        )}
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
            const batchReject = () =>
              rows.forEach((row) => {
                // 组级一键拒绝不带行级拒因（全空 → 服务端 NULL/NULL 留档）。
                if (row.dedupe_key != null && onReject) onReject(String(row.dedupe_key), [], "");
              });
            return (
              <details key={holder} className="lead-group" data-testid={`lead-group-${holder}`}>
                <summary className="lead-group-summary">
                  {holder} · {rows.length} 条
                  {onReject ? (
                    <button
                      className="badge"
                      data-testid={`lead-reject-all-${holder}`}
                      disabled={groupBusy}
                      onClick={(e) => {
                        // 组级钮在 summary 内：掐掉 <details> 默认开合，只有一击动作。
                        e.preventDefault();
                        e.stopPropagation();
                        batchReject();
                      }}
                    >
                      全拒
                    </button>
                  ) : null}
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
                    const chips = leadId ? (reasonChips[leadId] ?? []) : [];
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
                              // 一击即飞、无 dialog；提交行内当前已选拒因（全空合法）。
                              if (busyRejectId !== leadId) onReject(leadId, chips, note);
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
                              {(leads.reject_chip_reasons ?? []).map((chip) => (
                                <button
                                  key={chip}
                                  type="button"
                                  className={`badge${chips.includes(chip) ? " on" : ""}`}
                                  data-testid={`lead-reject-chip-${leadId}-${chip}`}
                                  disabled={busyRejectId === leadId}
                                  onClick={() => toggleChip(leadId, chip)}
                                >
                                  {chip}
                                </button>
                              ))}
                              <input
                                className="lead-note"
                                data-testid={`lead-reject-note-${leadId}`}
                                value={note}
                                maxLength={80}
                                disabled={busyRejectId === leadId}
                                placeholder="拒因注记（可选，≤80 字）"
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