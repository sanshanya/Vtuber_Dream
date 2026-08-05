/**
 * Z5/C1（终裁 P0-5）：制片人简报首屏卡——结论先行、句句带出处、沉默可呈现。
 *
 * 三种呈现态（T1 三态纪律在本卡的最小完整示范）：
 * 1. 未生成（situation 未 complete / 无 analysis）→ 空缺位 ≤ 首屏 40%：一句现状
 *    + 一键触发「主播 AI 分析」（KindRunButton kind="ai_audience"），不静默缺席；
 * 2. 沉默渠（analysis 在而 front_brief 缺席或 sentences 空）→ 「沉默可呈现」位：
 *    明示「本轮证据不足以成简报」——静默与无数据是两种可区分的知识状态；
 * 3. 就绪 → 句句带出处：每条 refs 以 episode_index 解析归属观众，可点跳个人树页；
 *    未解析的 ref（超 500 帽/旧缓存）退化为不可点 chip，绝不指向臆造目标。
 *
 * ag4-F6 同源纪律：analysis 是 LLM 产物无 schema 校验，所有取值过护栏（形状不符
 * 一律当沉默态处理，不抛）。
 *
 * 轮2-R1-B2：双查询（viewers/overview）下沉为 props——父页已持同 key 数据，
 * 卡不再自备数据源（纯呈现件，测试免 QueryClient/fetch stub）。
 */
import { fmtTime } from "../format";
import { AiStaleBadge } from "./AiStaleBadge";
import { KindRunButton } from "./KindRunButton";

interface BriefSentence {
  text: string;
  episodeRefs: string[];
  range: [string, string] | null;
}

export interface EpisodeIndexEntry {
  viewer_id?: string;
  title?: string | null;
}

/** LLM 形状护栏：Record 形状的 front_brief → 句子列表；任何漂移 → null（按沉默态渲染）。 */
export function parseBrief(analysis: Record<string, unknown> | undefined): BriefSentence[] | null {
  const brief = analysis?.front_brief;
  if (!brief || typeof brief !== "object" || Array.isArray(brief)) return null;
  const raw = (brief as Record<string, unknown>).sentences;
  if (!Array.isArray(raw)) return null;
  const sentences: BriefSentence[] = [];
  for (const item of raw) {
    if (!item || typeof item !== "object" || Array.isArray(item)) return null;
    const record = item as Record<string, unknown>;
    if (typeof record.text !== "string" || record.text.trim() === "") return null;
    if (!Array.isArray(record.episode_refs) || record.episode_refs.length === 0) return null;
    const episodeRefs = record.episode_refs.filter((r): r is string => typeof r === "string");
    const range = Array.isArray(record.coverage_time_range)
      ? (record.coverage_time_range.filter((v): v is string => typeof v === "string") as [
          string,
          string,
        ])
      : null;
    sentences.push({
      text: record.text,
      episodeRefs,
      range: range && range.length === 2 ? range : null,
    });
  }
  return sentences;
}

export function BriefingCard({
  analysis,
  situationStatus,
  aiCompletedAt,
  stale,
  episodeIndex,
  nameOf,
}: {
  analysis: Record<string, unknown> | undefined;
  situationStatus: string | undefined;
  aiCompletedAt?: string | null;
  /** 任一舰长信源已更新（ai_stale）→ 简报基底过时，盖时效章。 */
  stale: boolean;
  /** 归属解析面（overview.episode_index 透传）：episode_id → {viewer_id, title}。 */
  episodeIndex: Record<string, EpisodeIndexEntry>;
  /** 观众 uid → 显示名（viewers 行透传）。 */
  nameOf: ReadonlyMap<string, string>;
}) {
  const index = episodeIndex;
  const sentences = situationStatus === "complete" ? parseBrief(analysis) : null;

  // 态 1：未生成。
  if (situationStatus !== "complete" || analysis === undefined) {
    return (
      <section className="section card briefing-card" data-testid="briefing-card">
        <div className="section-title">
          <h2>制片人简报</h2>
          <span className="badge ai">AI 结论</span>
        </div>
        <div className="empty" data-testid="briefing-empty-slot">
          简报空缺——跑一轮「主播 AI 分析」后此处给出带出处的一句话结论。
        </div>
        <div className="action-bar">
          <KindRunButton kind="ai_audience" note="认知层：基于现有各舰长感知（幂等缓存）重推整体简报与行动建议。" />
        </div>
      </section>
    );
  }

  return (
    <section className="section card briefing-card" data-testid="briefing-card">
      <div className="section-title">
        <h2>制片人简报</h2>
        <span className="badge ai" title="front_brief 字段：结论先行、句句带出处（AI 推断层）">
          AI 结论
        </span>
        <span className="muted small" data-testid="briefing-timestamp">
          生成于 {fmtTime(aiCompletedAt)}
        </span>
        {stale && <AiStaleBadge testId="briefing-stale" />}
      </div>
      {sentences === null || sentences.length === 0 ? (
        // 态 2：沉默渠——AI 判断本轮证据不足以成简报，这本身是可见的结论。
        <div className="empty" data-testid="briefing-silent-slot">
          本轮证据不足以成简报（AI 沉默 = 有效结论）——补充采集或重跑「主播 AI 分析」。
        </div>
      ) : (
        // 态 3：就绪——句句带出处；refs 可点跳归属观众个人树。
        <ul className="brief-list" data-testid="briefing-list">
          {sentences.map((sentence, i) => (
            <li className="brief-item" key={i}>
              <p className="brief-text">{sentence.text}</p>
              <div className="brief-meta">
                {sentence.range && (
                  <span className="muted small" data-testid="brief-range">
                    覆盖 {sentence.range[0]} ~ {sentence.range[1]}
                  </span>
                )}
                {sentence.episodeRefs.map((ref) => {
                  const owner = index[ref];
                  const label = owner?.title?.trim() ? String(owner.title) : ref;
                  return owner?.viewer_id ? (
                    <a
                      key={ref}
                      className="brief-ref"
                      data-testid="brief-ref"
                      href={`#/viewers/${encodeURIComponent(owner.viewer_id)}/tree`}
                      title={`证据：${label}（${ref}）→ ${nameOf.get(owner.viewer_id) ?? owner.viewer_id} 的个人树`}
                    >
                      {label}
                    </a>
                  ) : (
                    <span key={ref} className="brief-ref unresolved" data-testid="brief-ref-unresolved" title={`未解析的 episode 引用：${ref}`}>
                      {ref}
                    </span>
                  );
                })}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
