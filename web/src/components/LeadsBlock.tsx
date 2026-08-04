/**
 * leads 区块（M4.x 公示面 + G2-B 审批缝）：五态计数徽标 + pending 明细直出。
 * 待审行带「批准」钮（一击即飞；data-testid=lead-approve-{dedupe_key}）——
 * 审批动作本身由宿主页（Leads）经 POST 审批缝承担，本块只做呈现与击发。
 * FE-F2：leads.summary 的 [lead_ledger] 裸文本不再上墙（人话面 = 下方五态徽标）；
 * 空账（五态全零）显式空态一句；待审行 key = dedupe_key（非索引）；
 * 在飞期间钮禁且 onClick 自身再做 busy 护栏（双击只发一次 POST）。
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
  /** G2-B：自治位读取面（0=纯人工 / 1=L1 自动批准+预算消费）。 */
  autonomy?: number;
  /** 账本直出行（leads.rs LedgerRow serde 形：type 重命名进 lead_type 由本层承担）。 */
  pending?: Array<{
    dedupe_key?: string;
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

export interface LeadsBlockProps {
  leads: LeadsView;
  /** 批准回调（宿主页持有 mutation）；缺省 = 纯读面不渲染钮。 */
  onApprove?: (leadId: string) => void;
  /** 在飞批准的目标 id（一击即飞的禁用面）。 */
  busyLeadId?: string | null;
}

export function LeadsBlock({ leads, onApprove, busyLeadId }: LeadsBlockProps) {
  const totals = leads.totals ?? {};
  const pending = leads.pending ?? [];
  // FE-F2/R1#1：空账 = 五态全零（summary 裸账本行不再上墙，人话面只留五态徽标）。
  const ledgerEmpty = TOTAL_LABELS.every(([key]) => (totals[key] ?? 0) === 0);
  return (
    <div>
      <div className="badges" data-testid="leads-totals">
        {TOTAL_LABELS.map(([key, label]) => (
          <span className="badge" key={key}>
            {label} {totals[key] ?? 0}
          </span>
        ))}
      </div>
      {ledgerEmpty && (
        <p className="empty" data-testid="leads-empty">
          暂无线索——主播 AI 分析或全量感知跑过后，可疑方向会落进这里。
        </p>
      )}
      {pending.length > 0 && (
        <ul className="delta-list">
          {pending.map((row, i) => {
            // ag4-F6：leads 系工具产物快照——入 JSX 的键一律 String() 护栏。
            const leadId = row.dedupe_key == null ? null : String(row.dedupe_key);
            return (
              // FE-F2/R3#7：列表 key = dedupe_key（身份随数据，不随索引漂移）。
              <li key={leadId ?? `no-key-${i}`}>
                <span className="badge fact">{String(row.type ?? "?")}</span>{" "}
                {String(row.locator ?? "?")}
                {row.viewer_id ? <span className="muted"> @{String(row.viewer_id)}</span> : null}
                {row.motivation ? <span className="muted"> · {String(row.motivation)}</span> : null}{" "}
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
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
