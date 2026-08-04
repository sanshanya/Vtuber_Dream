/**
 * 整体态势直渲（situation.analysis，键与 demo.rs / AudienceSituationSubmission 同源）。
 *
 * badge 四层调色纪律（ag1-F1 裁定）：推断=ai、状态判断=state、行动建议=action、
 * 平台事实=fact——计数徽标与逐项标注必须用同一层色（本组件是唯一实装点，
 * Dashboard 里的 count 行与 item 行致错史见 docs/review-m5/ag1）。
 * LLM 产物形状漂移无 schema 校验（ag4-F6）：所有入 JSX 的键一律 String() 护栏。
 */

function text(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

/** 计数徽标行类名表——vitest 唯一钉点，tail 头写出层名便于代码评审肉眼核对。 */
export const COUNT_BADGE_LAYERS = [
  { key: "interest_graph", label: "兴趣实体", badgeClass: "badge ai" }, // 推断层
  { key: "situations", label: "态势项", badgeClass: "badge state" }, // 状态判断层
  { key: "content_opportunities", label: "内容机会", badgeClass: "badge action" }, // 行动建议层
  { key: "content_calendar", label: "排期", badgeClass: "badge action" }, // 行动建议层（ag1-F1 修正：fact→action）
] as const;

export interface SituAnalysis {
  executive_summary?: unknown;
  interest_graph?: unknown;
  communities?: unknown;
  situations?: unknown;
  content_opportunities?: unknown;
  content_calendar?: unknown;
  [key: string]: unknown;
}

function asArray(value: unknown): Array<Record<string, unknown>> {
  return Array.isArray(value) ? (value as Array<Record<string, unknown>>) : [];
}

export function Situ({ analysis, synthetic = false }: { analysis: SituAnalysis; synthetic?: boolean }) {
  const interests = asArray(analysis.interest_graph);
  const communities = asArray(analysis.communities);
  const situations = asArray(analysis.situations);
  const opportunities = asArray(analysis.content_opportunities);
  const calendars = asArray(analysis.content_calendar);
  const counts: Record<string, number> = {
    interest_graph: interests.length,
    situations: situations.length,
    content_opportunities: opportunities.length,
    content_calendar: calendars.length,
  };
  return (
    <>
      {synthetic && (
        <span className="badge fact" data-testid="situ-synthetic">
          synthetic_demo 合成演示数据
        </span>
      )}
      {typeof analysis.executive_summary === "string" && <p>{analysis.executive_summary}</p>}
      <div className="badges" data-testid="situ-count-badges">
        {COUNT_BADGE_LAYERS.map((layer) => (
          <span className={layer.badgeClass} key={layer.key} data-layer={layer.key}>
            {layer.label} {counts[layer.key]}
          </span>
        ))}
      </div>
      {interests.map((item, i) => (
        <span className="badge ai" key={`g${i}`} title={text(item.evidence_summary)}>
          {text(item.entity, "?")}
          {text(item.status) ? ` · ${text(item.status)}` : ""}
        </span>
      ))}
      {communities.length > 0 && (
        <ul className="delta-list" data-testid="situ-communities">
          {communities.map((item, i) => (
            <li key={`c${i}`}>
              <strong>{text(item.name, "?")}</strong>
              <span className="badge state" style={{ margin: "0 6px" }}>
                {`观众 ${Array.isArray(item.viewer_ids) ? item.viewer_ids.length : 0}`}
              </span>
              <span className="muted">{text(item.description)}</span>
            </li>
          ))}
        </ul>
      )}
      {situations.length > 0 && (
        <ul className="delta-list">
          {situations.map((item, i) => (
            <li key={`s${i}`}>
              <strong>{text(item.title, "?")}</strong>
              <span className="badge state" style={{ margin: "0 6px" }}>
                {text(item.status, "?")}
              </span>
              <span className="muted">{text(item.description)}</span>
            </li>
          ))}
        </ul>
      )}
      {opportunities.length > 0 && (
        <ul className="delta-list">
          {opportunities.map((item, i) => (
            <li key={`o${i}`}>
              <strong>{text(item.title, text(item.entity, "?"))}</strong>
              {text(item.format) ? <span className="badge action">{text(item.format)}</span> : null}{" "}
              <span className="muted">{text(item.why_now)}</span>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
