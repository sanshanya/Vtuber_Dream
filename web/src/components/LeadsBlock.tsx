/**
 * leads 区块（M4.x 薄切公示面）：账本摘要行 + 五态计数 + pending 明细直出
 * （人工审批 = 编辑 leads.jsonl，面板只读——薄切设计有意为之）。
 */

export interface LeadsView {
  summary?: string;
  totals?: {
    pending_approval?: number;
    approved?: number;
    consumed?: number;
    rejected?: number;
    deferred?: number;
  };
  /** 账本直出行（leads.rs LedgerRow serde 形：type 重命名进 lead_type 由本层承担）。 */
  pending?: Array<{
    type?: string;
    locator?: string;
    viewer_id?: string;
    motivation?: string;
    priority?: string;
  }>;
}

const TOTAL_LABELS: Array<[keyof NonNullable<LeadsView["totals"]>, string]> = [
  ["pending_approval", "待审批"],
  ["approved", "已批准"],
  ["consumed", "已消费"],
  ["rejected", "已拒绝"],
  ["deferred", "暂缓"],
];

export function LeadsBlock({ leads }: { leads: LeadsView }) {
  const totals = leads.totals ?? {};
  const pending = leads.pending ?? [];
  return (
    <div>
      <p className="muted">{leads.summary ?? "—"}</p>
      <div className="badges">
        {TOTAL_LABELS.map(([key, label]) => (
          <span className="badge" key={key}>
            {label} {totals[key] ?? 0}
          </span>
        ))}
      </div>
      {pending.length > 0 && (
        <ul className="delta-list">
          {pending.map((row, i) => (
            <li key={i}>
              <span className="badge fact">{row.type ?? "?"}</span> {row.locator ?? "?"}
              {row.viewer_id ? <span className="muted"> @{row.viewer_id}</span> : null}
              {row.motivation ? <span className="muted"> · {row.motivation}</span> : null}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
