/**
 * 内容机会卡（audience 终局 `analysis.content_opportunities` 的呈现窗）。
 *
 * 为什么存在：产出面（situation.json）早已烧出钱——每张卡带标题/为何现在/
 * 为何适配/排场/话点/佐证计数，但面板此前只消费 front_brief 五句结论，
 * 机会卡没有窗。本卡只读呈现已落盘的认知产物，零新数据面。
 *
 * 三态纪律（与 BriefingCard 同型）：
 * 1. 未生成（situation 未 complete / 无 analysis）→ null 不渲染——
 *    「未生成」的话头归 BriefingCard 的空缺位，避免双卡说同一件事；
 * 2. 沉默渠（数组缺席/非数组/空）→ 显式「本轮证据不足以出机会」位；
 * 3. 就绪 → 卡列：标题 + 置信徽标 + 实体 chip，两段直陈，
 *    排场/话点/指标/风险与佐证计数折叠进 <details>（首屏不炸版面）。
 */

export interface Opportunity {
  title: string;
  entity: string | null;
  confidence: string | null;
  whyNow: string | null;
  whyFit: string | null;
  format: string | null;
  runOfShow: string[];
  talkingPoints: string[];
  observationMetrics: string[];
  caveats: string[];
  evidenceCount: number;
  searchCount: number;
  audienceCount: number;
}

function strList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string" && v !== "") : [];
}

/** LLM 形状护栏：content_opportunities → 机会卡列表；任何漂移 → null（按沉默态渲染）。 */
export function parseOpportunities(analysis: Record<string, unknown> | undefined): Opportunity[] | null {
  const raw = analysis?.content_opportunities;
  if (raw === undefined) return null;
  if (!Array.isArray(raw)) return null;
  const out: Opportunity[] = [];
  for (const item of raw) {
    if (!item || typeof item !== "object") continue;
    const rec = item as Record<string, unknown>;
    if (typeof rec.title !== "string" || rec.title.trim() === "") continue;
    out.push({
      title: rec.title.trim(),
      entity: typeof rec.entity === "string" && rec.entity !== "" ? rec.entity : null,
      confidence:
        typeof rec.confidence === "string" && rec.confidence !== "" ? rec.confidence : null,
      whyNow: typeof rec.why_now === "string" && rec.why_now !== "" ? rec.why_now : null,
      whyFit: typeof rec.why_fit === "string" && rec.why_fit !== "" ? rec.why_fit : null,
      format: typeof rec.format === "string" && rec.format !== "" ? rec.format : null,
      runOfShow: strList(rec.run_of_show),
      talkingPoints: strList(rec.talking_points),
      observationMetrics: strList(rec.observation_metrics),
      caveats: strList(rec.caveats),
      evidenceCount: strList(rec.evidence_mention_ids).length,
      searchCount: strList(rec.search_result_ids).length,
      audienceCount: strList(rec.audience_ids).length,
    });
  }
  return out;
}

export function OpportunitiesCard({
  situationStatus,
  analysis,
}: {
  situationStatus: string | undefined;
  analysis: Record<string, unknown> | undefined;
}) {
  // 态 1：未生成——静默退场（BriefingCard 已具名呈现「未生成」）。
  if (situationStatus !== "complete" || analysis === undefined) {
    return null;
  }
  const opportunities = parseOpportunities(analysis);

  return (
    <section className="section card" data-testid="opportunities-card">
      <div className="section-title">
        <h2>内容机会</h2>
        <span className="badge ai" title="content_opportunities 字段：带证据的周级动作建议（AI 推断层）">
          AI 建议
        </span>
      </div>
      {opportunities === null || opportunities.length === 0 ? (
        // 态 2：沉默渠——AI 本轮没给机会；沉默是有效结论，不造假卡。
        <div className="empty" data-testid="opportunities-silent-slot">
          本轮证据不足以出内容机会（AI 沉默 = 有效结论）——补充采集或重跑「主播 AI 分析」。
        </div>
      ) : (
        // 态 3：就绪。
        <ul className="brief-list" data-testid="opportunities-list">
          {opportunities.map((op, i) => (
            <li className="brief-item" key={i} data-testid="opportunity-item">
              <div className="brief-meta">
                <strong>{op.title}</strong>
                {op.confidence && (
                  <span className="badge" data-testid={`opportunity-confidence-${i}`}>
                    置信 {op.confidence}
                  </span>
                )}
                {op.entity && <span className="brief-ref">{op.entity}</span>}
              </div>
              {op.whyNow && <p className="brief-text">为何是现在：{op.whyNow}</p>}
              {op.whyFit && <p className="brief-text">为何适合本房：{op.whyFit}</p>}
              <details data-testid={`opportunity-detail-${i}`}>
                <summary className="muted small">
                  排场/话点/观察指标/风险 · 覆盖观众 {op.audienceCount} · 证据 {op.evidenceCount} 条 ·
                  搜索佐证 {op.searchCount} 条
                </summary>
                {op.format && <p className="brief-text">形式：{op.format}</p>}
                {op.runOfShow.length > 0 && (
                  <>
                    <p className="muted small">排场</p>
                    <ul>
                      {op.runOfShow.map((s, j) => (
                        <li key={j}>{s}</li>
                      ))}
                    </ul>
                  </>
                )}
                {op.talkingPoints.length > 0 && (
                  <>
                    <p className="muted small">话点</p>
                    <ul>
                      {op.talkingPoints.map((s, j) => (
                        <li key={j}>{s}</li>
                      ))}
                    </ul>
                  </>
                )}
                {op.observationMetrics.length > 0 && (
                  <>
                    <p className="muted small">观察指标</p>
                    <ul>
                      {op.observationMetrics.map((s, j) => (
                        <li key={j}>{s}</li>
                      ))}
                    </ul>
                  </>
                )}
                {op.caveats.length > 0 && (
                  <>
                    <p className="muted small">风险与边界</p>
                    <ul>
                      {op.caveats.map((s, j) => (
                        <li key={j}>{s}</li>
                      ))}
                    </ul>
                  </>
                )}
              </details>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
