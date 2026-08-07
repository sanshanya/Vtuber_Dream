/**
 * 态势详面卡——audience 终局 `analysis` 剩余六个面的呈现窗：
 * 执行摘要 / 内容日历 / 话题场 / 观众结构 / 社群 / 个人亮点 / 缺口 / 避险。
 *
 * 为什么存在：这些面已随每轮烧钱产出并落盘，此前面板只消费 front_brief
 * 五句 + content_opportunities 卡——其余八键躺抽屉。用户裁决：「做了就要展示」。
 *
 * 纪律：
 * - situation 未 complete / 无 analysis → null 不渲染（话头归 BriefingCard）；
 * - 每面形状护栏独立：漂移/缺席 → 「本轮无」muted 行，不造假块不炸卡；
 * - 列表面折叠进 <details>（summary 携计数），首屏版面不被详情炸开；
 * - 个人亮点的观众 chip 复用简报卡语义：可点跳 `#/viewers/{uid}/tree`。
 */

interface CalendarRow {
  session: string;
  theme: string | null;
  goal: string | null;
  signal: string | null;
  targetCount: number;
}

interface SituationRow {
  title: string;
  status: string | null;
  description: string | null;
  triggerCount: number;
  evidenceCount: number;
  viewerCount: number;
}

interface CommunityRow {
  name: string;
  description: string | null;
  sharedAngles: string[];
  viewerCount: number;
  evidenceCount: number;
  confidence: number | null;
}

interface HighlightRow {
  viewerId: string;
  insight: string | null;
  opportunity: string | null;
  evidenceCount: number;
}

export interface SituationDetail {
  executiveSummary: string | null;
  calendar: CalendarRow[] | null;
  situations: SituationRow[] | null;
  structureLines: string[];
  communities: CommunityRow[] | null;
  highlights: HighlightRow[] | null;
  dataGaps: string[];
  safetyNotes: string[];
}

function strOrNull(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null;
}

function strList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string" && v !== "") : [];
}

function records(value: unknown): Record<string, unknown>[] | null {
  if (value === undefined) return null;
  if (!Array.isArray(value)) return null;
  return value.filter(
    (item): item is Record<string, unknown> => !!item && typeof item === "object",
  );
}

/** LLM 形状护栏：逐面独立——哪面漂移哪面落 null（该块按「本轮无」呈现），不连坐。 */
export function parseSituationDetail(analysis: Record<string, unknown> | undefined): SituationDetail {
  const cal = records(analysis?.content_calendar)?.map((rec) => ({
    session: strOrNull(rec.session) ?? "（未命名场次）",
    theme: strOrNull(rec.theme),
    goal: strOrNull(rec.goal),
    signal: strOrNull(rec.validation_signal),
    targetCount: strList(rec.target_viewers).length,
  }));

  const sits = records(analysis?.situations)
    ?.map((rec) => {
      const title = strOrNull(rec.title);
      if (!title) return null;
      return {
        title,
        status: strOrNull(rec.status),
        description: strOrNull(rec.description),
        triggerCount: strList(rec.trigger_events).length,
        evidenceCount: strList(rec.evidence_mention_ids).length,
        viewerCount: strList(rec.viewer_ids).length,
      };
    })
    .filter((row): row is SituationRow => row !== null);

  const comms = records(analysis?.communities)
    ?.map((rec) => {
      const name = strOrNull(rec.name);
      if (!name) return null;
      return {
        name,
        description: strOrNull(rec.description),
        sharedAngles: strList(rec.shared_angles),
        viewerCount: strList(rec.viewer_ids).length,
        evidenceCount: strList(rec.evidence_mention_ids).length,
        confidence: typeof rec.confidence === "number" ? rec.confidence : null,
      };
    })
    .filter((row): row is CommunityRow => row !== null);

  const highs = records(analysis?.individual_highlights)
    ?.map((rec) => {
      const viewerId = strOrNull(rec.viewer_id);
      if (!viewerId) return null;
      return {
        viewerId,
        insight: strOrNull(rec.insight),
        opportunity: strOrNull(rec.opportunity),
        evidenceCount: strList(rec.evidence_mention_ids).length,
      };
    })
    .filter((row): row is HighlightRow => row !== null);

  return {
    executiveSummary: strOrNull(analysis?.executive_summary),
    calendar: cal ?? null,
    situations: sits ?? null,
    structureLines: strList(analysis?.audience_structure),
    communities: comms ?? null,
    highlights: highs ?? null,
    dataGaps: strList(analysis?.data_gaps),
    safetyNotes: strList(analysis?.safety_notes),
  };
}

/** 单面行：非空 → <details> 计数折叠；空/漂移 → 「本轮无」muted 行（缺席必可见）。 */
function Face({
  label,
  count,
  children,
  testId,
}: {
  label: string;
  count: number | null;
  children?: React.ReactNode;
  testId: string;
}) {
  if (count === null || count === 0) {
    return (
      <p className="muted small" data-testid={`${testId}-none`}>
        {label} · 本轮无
      </p>
    );
  }
  return (
    <details data-testid={testId}>
      <summary>
        {label} · {count} 条
      </summary>
      {children}
    </details>
  );
}

export function SituationDetailCard({
  situationStatus,
  analysis,
  nameOf,
}: {
  situationStatus: string | undefined;
  analysis: Record<string, unknown> | undefined;
  nameOf: Map<string, string>;
}) {
  if (situationStatus !== "complete" || analysis === undefined) {
    return null;
  }
  const detail = parseSituationDetail(analysis);

  return (
    <section className="section card" data-testid="situation-detail-card">
      <div className="section-title">
        <h2>态势详面</h2>
        <span className="badge ai" title="audience 终局 analysis 的其余八个面（AI 推断层）">
          AI 推断
        </span>
      </div>

      {detail.executiveSummary === null ? (
        <p className="muted small" data-testid="executive-summary-none">
          执行摘要 · 本轮无
        </p>
      ) : (
        <p className="brief-text" data-testid="executive-summary">
          {detail.executiveSummary}
        </p>
      )}

      <Face label="内容日历" count={detail.calendar === null ? null : detail.calendar.length} testId="face-calendar">
        <ul>
          {detail.calendar?.map((row, i) => (
            <li key={i}>
              <strong>{row.session}</strong>
              {row.theme && `：${row.theme}`}
              {row.goal && <span className="muted small">（目标：{row.goal}）</span>}
              {row.signal && <span className="muted small">｜验收信号：{row.signal}</span>}
              <span className="muted small">｜面向 {row.targetCount} 位观众</span>
            </li>
          ))}
        </ul>
      </Face>

      <Face label="话题场" count={detail.situations === null ? null : detail.situations.length} testId="face-situations">
        <ul>
          {detail.situations?.map((row, i) => (
            <li key={i}>
              <strong>{row.title}</strong>
              {row.status && <span className="badge">{row.status}</span>}
              {row.description && <p className="brief-text">{row.description}</p>}
              <span className="muted small">
                触发 {row.triggerCount} · 涉 {row.viewerCount} 人 · 证据 {row.evidenceCount} 条
              </span>
            </li>
          ))}
        </ul>
      </Face>

      <Face
        label="观众结构"
        count={detail.structureLines.length === 0 ? 0 : detail.structureLines.length}
        testId="face-structure"
      >
        <ul>
          {detail.structureLines.map((line, i) => (
            <li key={i}>{line}</li>
          ))}
        </ul>
      </Face>

      <Face label="社群" count={detail.communities === null ? null : detail.communities.length} testId="face-communities">
        <ul>
          {detail.communities?.map((row, i) => (
            <li key={i}>
              <strong>{row.name}</strong>
              {row.confidence !== null && <span className="muted small">（置信 {row.confidence}）</span>}
              {row.description && <p className="brief-text">{row.description}</p>}
              {row.sharedAngles.length > 0 && (
                <p className="muted small">共同切口：{row.sharedAngles.join("、")}</p>
              )}
              <span className="muted small">
                {row.viewerCount} 人 · 证据 {row.evidenceCount} 条
              </span>
            </li>
          ))}
        </ul>
      </Face>

      <Face label="个人亮点" count={detail.highlights === null ? null : detail.highlights.length} testId="face-highlights">
        <ul>
          {detail.highlights?.map((row, i) => (
            <li key={i}>
              <a className="brief-ref" href={`#/viewers/${encodeURIComponent(row.viewerId)}/tree`}>
                {nameOf.get(row.viewerId) ?? row.viewerId}
              </a>
              {row.insight && <p className="brief-text">{row.insight}</p>}
              {row.opportunity && <p className="muted small">机会：{row.opportunity}</p>}
              <span className="muted small">证据 {row.evidenceCount} 条</span>
            </li>
          ))}
        </ul>
      </Face>

      <Face label="证据缺口" count={detail.dataGaps.length === 0 ? 0 : detail.dataGaps.length} testId="face-gaps">
        <ul>
          {detail.dataGaps.map((line, i) => (
            <li key={i}>{line}</li>
          ))}
        </ul>
      </Face>

      <Face label="避险" count={detail.safetyNotes.length === 0 ? 0 : detail.safetyNotes.length} testId="face-safety">
        <ul>
          {detail.safetyNotes.map((line, i) => (
            <li key={i}>{line}</li>
          ))}
        </ul>
      </Face>
    </section>
  );
}
