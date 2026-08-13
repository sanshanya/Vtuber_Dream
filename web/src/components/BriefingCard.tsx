/**
 * 制片人简报首屏卡（终裁）——结论先行、句句带出处、沉默可呈现。
 *
 * 三种呈现态（T1 三态纪律在本卡的最小完整示范）：
 * 1. 未生成（situation 未 complete / 无 analysis）→ 空缺位 ≤ 首屏 40%：一句现状
 *    + 一键触发「主播 AI 分析」（KindRunButton kind="ai_audience"），不静默缺席；
 * 2. 沉默渠（analysis 在而 front_brief 缺席或 sentences 空）→ 「沉默可呈现」位：
 *    明示「本轮证据不足以成简报」——静默与无数据是两种可区分的知识状态；
 * 3. 就绪 → 句句带出处：每条 refs 以 episode_index 解析归属观众，可点跳个人树页；
 *    未解析的 ref（超 500 帽/旧缓存）退化为不可点 chip，绝不指向臆造目标。
 *
 * 同源纪律：analysis 是 LLM 产物无 schema 校验，所有取值过护栏（形状不符
 * 一律当沉默态处理，不抛）。
 *
 * 双查询（viewers/overview）下沉为 props——父页已持同 key 数据，
 * 卡不再自备数据源（纯呈现件，测试免 QueryClient/fetch stub）。
 */
import type { SituationFailureView } from "../api";
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
  /** Episode 源（video/dynamic/…/live_ws_*）——芯片副文本类型词的素材。 */
  source?: string | null;
}

/** 类型词表：芯片副文本=「什么」（Episode 类型），未知源原样落、缺席=空串
 *  （芯片只落人名，绝不臆造类型）。 */
export function episodeKindWord(source: string | null | undefined): string {
  switch (source ?? "") {
    case "video":
      return "投稿";
    case "dynamic":
      return "动态";
    case "favorite":
      return "收藏";
    case "bangumi":
      return "追番";
    case "live_danmaku":
    case "live_ws_danmaku":
      return "弹幕";
    case "room_comment":
      return "评论";
    case "live_ws_sc":
      return "醒目留言";
    case "live_ws_entry":
      return "进场";
    default:
      return source?.trim() ?? "";
  }
}

interface RefGroup {
  viewerId: string | null;
  refs: string[];
}

/** refs 按证据持有人（viewer_id）归并为一组，保首次出现序；未解析（无归属面）
 *  的 ref 单独成组、viewerId=null——降级渲染权归调用处。 */
export function groupRefsByHolder(
  refs: string[],
  index: Record<string, EpisodeIndexEntry>,
): RefGroup[] {
  const groups: RefGroup[] = [];
  const position = new Map<string, number>();
  for (const ref of refs) {
    const viewerId = index[ref]?.viewer_id ?? null;
    const key = viewerId ?? `unresolved:${ref}`;
    const slot = position.get(key);
    if (slot === undefined) {
      position.set(key, groups.length);
      groups.push({ viewerId, refs: [ref] });
    } else {
      groups[slot].refs.push(ref);
    }
  }
  return groups;
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
  lastFailure,
  episodeIndex,
  nameOf,
}: {
  analysis: Record<string, unknown> | undefined;
  situationStatus: string | undefined;
  aiCompletedAt?: string | null;
  /** 任一舰长信源已更新（ai_stale）→ 简报基底过时，盖时效章。 */
  stale: boolean;
  /** 败态旁车档（overview.situation_last_failure 透传）：存在 ≡ 最近一轮败。 */
  lastFailure?: SituationFailureView | null;
  /** 归属解析面（overview.episode_index 透传）：episode_id → {viewer_id, title}。 */
  episodeIndex: Record<string, EpisodeIndexEntry>;
  /** 观众 uid → 显示名（viewers 行透传）。 */
  nameOf: ReadonlyMap<string, string>;
}) {
  const index = episodeIndex;
  const sentences = situationStatus === "complete" ? parseBrief(analysis) : null;
  // 败因徽标素材：两键缺一即静默（形状漂移按无档处理——缺席即无纪律）。
  const failureNote =
    lastFailure && typeof lastFailure.error === "string" && lastFailure.error.trim() !== ""
      ? { error: lastFailure.error.trim(), at: lastFailure.failed_at }
      : null;

  // 态 1：未生成。
  if (situationStatus !== "complete" || analysis === undefined) {
    return (
      <section className="section card briefing-card" data-testid="briefing-card">
        <div className="section-title">
          <h2>制片人简报</h2>
          <span className="badge ai">AI 结论</span>
        </div>
        <div className="empty" data-testid="briefing-empty-slot">
          {/* 败因全量直呈（截断会把病因截走——80 字掐头正是"request build"
              被掐掉的教训）；长文本自然折行。 */}
          {failureNote ? (
            <span data-testid="briefing-failure-note">
              简报未生成——上一轮刷新失败（{fmtTime(failureNote.at)}）：
              {failureNote.error}。跑一轮「主播 AI 分析」重试。
            </span>
          ) : (
            "简报空缺——跑一轮「主播 AI 分析」后此处给出带出处的一句话结论。"
          )}
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
        {failureNote && (
          // 优态保全章：上方成品是上次成果，最近一轮刷新失败——败因入 title 全量留档。
          <span
            className="badge warn"
            data-testid="briefing-failure-note"
            title={failureNote.error}
          >
            上轮刷新失败（{fmtTime(failureNote.at)}）——本卡为上次成品
          </span>
        )}
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
                {/* 芯片=「谁说的·什么」——同一持有人多证据合并一芯携计数；
                    标签=持有人名·类型词（不再拿 Episode 标题冒充持有人），点击行为不变
                    （入树并定位），title 载全量 ref 编号。未解析 ref 保持原样降级。 */}
                {groupRefsByHolder(sentence.episodeRefs, index).map((group) => {
                  if (group.viewerId === null) {
                    return group.refs.map((ref) => (
                      <span key={ref} className="brief-ref unresolved" data-testid="brief-ref-unresolved" title={`未解析的 episode 引用：${ref}`}>
                        {ref}
                      </span>
                    ));
                  }
                  const uid = group.viewerId;
                  const name = nameOf.get(uid) ?? uid;
                  const kinds = [...new Set(group.refs.map((ref) => episodeKindWord(index[ref]?.source)).filter((word) => word !== ""))];
                  const label = `${name}${kinds.length > 0 ? `·${kinds.join("/")}` : ""}${group.refs.length > 1 ? `×${group.refs.length}` : ""}`;
                  return (
                    <a
                      key={uid}
                      className="brief-ref"
                      data-testid="brief-ref"
                      href={`#/viewers/${encodeURIComponent(uid)}/tree`}
                      title={`证据 ${group.refs.length} 条：${group.refs.join("、")} → ${name} 的个人树`}
                    >
                      {label}
                    </a>
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
