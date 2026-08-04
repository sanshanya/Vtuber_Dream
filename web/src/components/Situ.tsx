/**
 * 整体态势直渲（situation.analysis，键与 demo.rs / AudienceSituationSubmission 同源）。
 *
 * badge 四层调色纪律（ag1-F1 裁定）：推断=ai、状态判断=state、行动建议=action、
 * 平台事实=fact——计数徽标与逐项标注必须用同一层色（本组件是唯一实装点，
 * 旧 Dashboard 里的 count 行与 item 行致错史见 docs/review-m5/ag1）。
 * LLM 产物形状漂移无 schema 校验（ag4-F6）：所有入 JSX 的键一律 String() 护栏。
 * Z2：executive_summary 走 Markdown 渲染（旧实现整段入 <p>，##/** 记号漏上墙）。
 * Z2b：单卡 1595px 墙被用户点名——按语义部件拆卡（摘要/兴趣实体/社群/关键态势/
 * 行动建议+排期）；content_calendar 补渲染（旧实现只计数、列表缺席）。
 */
import { Markdown } from "./Markdown";

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
  // AI 摘要常以「## 整体态势」开场——与卡片标题重复，剥掉首个同名标题行。
  const summary =
    typeof analysis.executive_summary === "string"
      ? analysis.executive_summary.replace(/^\s*#{1,3}\s*整体态势\s*\r?\n?/, "")
      : "";
  return (
    <>
      <section className="section card situ-part">
        <div className="section-title">
          <h2>态势摘要</h2>
        </div>
        {/* W2/r1-F1：合成标记是「反事实」元信息，不穿 fact 层色——裸 badge 无层色。
            四层调色只服务 事实/推断/状态/行动 的内容分层，元标记不入层。 */}
        {synthetic && (
          <span className="badge" data-testid="situ-synthetic">
            synthetic_demo 合成演示数据
          </span>
        )}
        {summary.length > 0 && <Markdown text={summary} />}
        <div className="badges" data-testid="situ-count-badges">
          {COUNT_BADGE_LAYERS.map((layer) => (
            <span className={layer.badgeClass} key={layer.key} data-layer={layer.key}>
              {layer.label} {counts[layer.key]}
            </span>
          ))}
        </div>
      </section>

      {interests.length > 0 && (
        // Z3：兴趣实体表化（旧版站「具体兴趣图」五列结构收敛到我们的三实键：实体/状态/关注角度——
        // 涉及观众/置信度不在 audience 提交键形里，不臆造补上）。
        <section className="section card situ-part">
          <div className="section-title">
            <h2>兴趣实体</h2>
            <span className="badge ai">{interests.length} 项</span>
          </div>
          <div className="table-wrap">
            <table className="data-table" data-testid="situ-interests">
              <thead>
                <tr>
                  <th>实体</th>
                  <th>状态</th>
                  <th>关注角度（证据据点）</th>
                </tr>
              </thead>
              <tbody>
                {interests.map((item, i) => (
                  <tr key={`g${i}`}>
                    <td>
                      <strong>{text(item.entity, "?")}</strong>
                    </td>
                    <td>
                      <span className="badge state">{text(item.status, "未标")}</span>
                    </td>
                    <td className="muted">{text(item.evidence_summary)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      {communities.length > 0 && (
        <section className="section situ-part">
          <div className="section-title">
            <h2>观众社群</h2>
            <span className="badge ai">{communities.length} 个</span>
          </div>
          {/* Z3 卡格化（旧版站 article.card 结构）：单社群=单卡。 */}
          <div className="grid three" data-testid="situ-communities">
            {communities.map((item, i) => (
              <article className="card" key={`c${i}`}>
                {/* W2/r1-F5：communities 是 audience 提交的 AI 群体推断产物 → 推断层色。 */}
                <div className="badges">
                  <span className="badge ai">
                    {`观众 ${Array.isArray(item.viewer_ids) ? item.viewer_ids.length : 0}`}
                  </span>
                </div>
                <h3>{text(item.name, "?")}</h3>
                <p>{text(item.description)}</p>
              </article>
            ))}
          </div>
        </section>
      )}

      {situations.length > 0 && (
        <section className="section situ-part">
          <div className="section-title">
            <h2>关键态势</h2>
            <span className="badge state">{situations.length} 项</span>
          </div>
          <div className="grid three">
            {situations.map((item, i) => (
              <article className="card" key={`s${i}`}>
                <div className="badges">
                  <span className="badge state">{text(item.status, "?")}</span>
                </div>
                <h3>{text(item.title, "?")}</h3>
                <p>{text(item.description)}</p>
              </article>
            ))}
          </div>
        </section>
      )}

      {(opportunities.length > 0 || calendars.length > 0) && (
        <section className="section situ-part">
          <div className="section-title">
            <h2>行动建议与排期</h2>
            <span className="badge action">
              {opportunities.length + calendars.length} 项
            </span>
          </div>
          <div className="grid two">
            {opportunities.map((item, i) => (
              <article className="card" key={`o${i}`}>
                <div className="badges">
                  {text(item.format) ? <span className="badge action">{text(item.format)}</span> : null}
                </div>
                <h3>{text(item.title, text(item.entity, "?"))}</h3>
                <p>{text(item.why_now)}</p>
              </article>
            ))}
            {/* Z2b：content_calendar 旧实现只计数不呈现——排期项与建议同格同形。
                多卡同 testid 属 RTL 的 queryAll* 语义（排期钉按 queryAllByTestId 取）。 */}
            {calendars.map((item, i) => (
              <article className="card" key={`k${i}`} data-testid="situ-calendar">
                <div className="badges">
                  {text(item.session) ? <span className="badge action">{text(item.session)}</span> : null}
                </div>
                <h3>{text(item.theme, "?")}</h3>
                <p>{text(item.goal)}</p>
                {text(item.validation_signal) ? (
                  <div className="small muted">验证信号：{text(item.validation_signal)}</div>
                ) : null}
              </article>
            ))}
          </div>
        </section>
      )}
    </>
  );
}
