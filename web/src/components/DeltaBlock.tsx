/**
 * delta 区块（D4 口径）：「vs 上轮」= 相邻两次 complete 运行的兴趣态 + 舰长差分。
 * 首轮/单边情形一律显示「基线已建」（design 原文，baseline_only=true）。
 *
 * 形状与 live-core graph/query.rs run_pair_delta 输出逐键同源：
 *   interest.opened/closed: {viewer_id, entity_id, canonical_name, status, preference}
 *   interest.changed:       {viewer_id, entity_id, canonical_name, from:{status,preference}, to:{…}}
 *   guards.added/removed:   viewer_id 字符串数组
 */

export interface DeltaView {
  baseline_only?: boolean;
  from_run_id?: string | null;
  to_run_id?: string | null;
  interest?: {
    opened?: DeltaInterest[];
    closed?: DeltaInterest[];
    changed?: DeltaChange[];
  };
  guards?: { added?: string[]; removed?: string[] };
}

export interface DeltaInterest {
  viewer_id?: string | null;
  entity_id?: string | null;
  canonical_name?: string | null;
  status?: string | null;
  preference?: string | null;
}

export interface DeltaChange {
  viewer_id?: string | null;
  entity_id?: string | null;
  canonical_name?: string | null;
  from?: { status?: string | null; preference?: string | null };
  to?: { status?: string | null; preference?: string | null };
}

function nameOf(item: { canonical_name?: string | null; entity_id?: string | null }): string {
  return item.canonical_name ?? item.entity_id ?? "?";
}

function ownerOf(item: { viewer_id?: string | null }): string {
  return item.viewer_id ? `@${item.viewer_id}` : "";
}

function changeLabel(item: DeltaChange): string {
  const piece = (key: "status" | "preference") => {
    const from = item.from?.[key];
    const to = item.to?.[key];
    return from !== to && (from || to) ? `${key} ${from ?? "—"}→${to ?? "—"}` : null;
  };
  return [piece("status"), piece("preference")].filter(Boolean).join(" · ");
}

export function DeltaBlock({ delta }: { delta: DeltaView }) {
  if (delta.baseline_only) {
    // D4：首轮/单边 → 基线态徽章，不渲染空表格。
    return (
      <div data-testid="delta-baseline" className="delta-block">
        <span className="badge state">基线已建</span>
        <span className="muted"> 与本轮的差分需 ≥2 次完整运行后呈现</span>
      </div>
    );
  }
  const interest = delta.interest ?? {};
  const guards = delta.guards ?? {};
  return (
    <div data-testid="delta-diff" className="delta-block">
      <div className="grid two">
        <div>
          <h3>兴趣态迁移</h3>
          <div className="badges">
            <span className="badge action">新增 {interest.opened?.length ?? 0}</span>
            <span className="badge danger">关闭 {interest.closed?.length ?? 0}</span>
            <span className="badge ai">迁移 {interest.changed?.length ?? 0}</span>
          </div>
          <ul className="delta-list">
            {(interest.opened ?? []).map((item, i) => (
              <li key={`o${i}`}>
                ＋ {nameOf(item)} <span className="muted">{ownerOf(item)}</span>
              </li>
            ))}
            {(interest.closed ?? []).map((item, i) => (
              <li key={`c${i}`}>
                − {nameOf(item)} <span className="muted">{ownerOf(item)}</span>
              </li>
            ))}
            {(interest.changed ?? []).map((item, i) => (
              <li key={`m${i}`}>
                ↻ {nameOf(item)} <span className="muted">{ownerOf(item)}</span>{" "}
                {changeLabel(item)}
              </li>
            ))}
          </ul>
        </div>
        <div>
          <h3>舰长名单</h3>
          <div className="badges">
            <span className="badge action">＋{guards.added?.length ?? 0}</span>
            <span className="badge danger">−{guards.removed?.length ?? 0}</span>
          </div>
          <ul className="delta-list">
            {(guards.added ?? []).map((uid) => (
              <li key={`ga${uid}`}>＋ {uid}</li>
            ))}
            {(guards.removed ?? []).map((uid) => (
              <li key={`gr${uid}`}>− {uid}</li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}
