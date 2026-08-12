import { useQuery } from "@tanstack/react-query";

import { api, type EpisodeRow, type MentionRow } from "../api";
import { AiStaleBadge } from "../components/AiStaleBadge";
import { Avatar } from "../components/Avatar";
import { MentionText } from "../components/MentionText";
import { SingleViewerRunButton } from "../components/SingleViewerRunButton";
import { fmtTime } from "../format";

/** LLM 可执行建议件（conversation_openers / content_ideas 共有形状）。 */
type ActionItem = { title: string; detail: string; evidence: number };

const asStrList = (value: unknown): string[] =>
  Array.isArray(value)
    ? value.filter((x): x is string => typeof x === "string" && x.trim() !== "")
    : [];

const asActions = (value: unknown): ActionItem[] =>
  Array.isArray(value)
    ? value
        .map((x) => {
          if (typeof x !== "object" || x === null) return null;
          const rec = x as Record<string, unknown>;
          const title = typeof rec.title === "string" ? rec.title : "";
          const detail = typeof rec.detail === "string" ? rec.detail : "";
          const evidence = Array.isArray(rec.evidence_mention_ids)
            ? rec.evidence_mention_ids.length
            : 0;
          return title || detail ? { title, detail, evidence } : null;
        })
        .filter((x): x is ActionItem => x !== null)
    : [];

/** 画像散文折段：先按「整体画像」类收口锚分意群（有锚才分），群内再按句号
 * 边界编组为均匀小段（约 PARA_CHARS 字一段、只切在句末标点后，不丢字不改序）；
 * 无收口锚 + 无句号 → 原样单段（呈现层容许失败）。 */
const PARA_CHARS = 100;

const SENTENCE_RE = /[^。！？!?]+[。！？!?]+[‘”’”』」》）)】]?/g;

const groupSentences = (part: string): string[] => {
  const sentences = part.match(SENTENCE_RE) ?? [part];
  const groups: string[] = [];
  let current = "";
  for (const sentence of sentences) {
    current += sentence;
    if (current.length >= PARA_CHARS) {
      groups.push(current.trim());
      current = "";
    }
  }
  if (current.trim() !== "") groups.push(current.trim());
  return groups.filter((group) => group !== "");
};

const splitSummary = (text: string): string[] =>
  text
    .split(/(?=(?:整体画像|整体来看|总体而言|综合来看|综合画像)[：:])/)
    .flatMap((part) => groupSentences(part.trim()))
    .filter((part) => part !== "");

function ActionCard({ item, testId }: { item: ActionItem; testId: string }) {
  return (
    <div className="action-card" data-testid={testId}>
      <div className="action-title">{item.title}</div>
      {item.detail !== "" && <div className="action-detail">{item.detail}</div>}
      {item.evidence > 0 && (
        <div className="badges action-evi">
          <span className="badge fact">证据×{item.evidence}</span>
        </div>
      )}
    </div>
  );
}

/** AI 感知结构化块（R3 二次加工）：submission 的九个结构化字段分块落座，
 *  散文只留画像一段；空块整体隐身（缺席即无，不臆造分区）。 */
function AiPerceptionCard({ analysis }: { analysis: Record<string, unknown> }) {
  const summary = typeof analysis.profile_summary === "string" ? analysis.profile_summary : "";
  const paras = summary === "" ? [] : splitSummary(summary);
  const prefs = asStrList(analysis.content_preferences);
  const changes = asStrList(analysis.recent_changes);
  const hypotheses = asStrList(analysis.hypotheses);
  const openers = asActions(analysis.conversation_openers);
  const ideas = asActions(analysis.content_ideas);
  const cautions = asStrList(analysis.cautions);
  const enrichments = asStrList(analysis.enrichment_targets);
  if (
    [paras, prefs, changes, hypotheses, openers, ideas, cautions, enrichments].every(
      (list) => list.length === 0,
    )
  ) {
    return null;
  }
  return (
    <div className="card ai-perception" data-testid="ai-perception-card">
      <div className="badges">
        <span className="badge ai">AI 推断 · 非事实面</span>
      </div>
      {paras.length > 0 && (
        <div className="prose" data-testid="profile-summary">
          {paras.map((para, index) => (
            <p key={index}>{para}</p>
          ))}
        </div>
      )}
      {prefs.length > 0 && (
        <section className="perception-block" data-testid="block-prefs">
          <h4>内容偏好（{prefs.length}）</h4>
          <div className="chips">
            {prefs.map((x, i) => (
              <span className="chip" key={i}>
                {x}
              </span>
            ))}
          </div>
        </section>
      )}
      {changes.length > 0 && (
        <section className="perception-block" data-testid="block-changes">
          <h4>近期变化（{changes.length}）</h4>
          <ul className="perception-list">
            {changes.map((x, i) => (
              <li key={i}>{x}</li>
            ))}
          </ul>
        </section>
      )}
      {openers.length > 0 && (
        <section className="perception-block" data-testid="block-openers">
          <h4>可聊开场（{openers.length}）</h4>
          {openers.map((item, i) => (
            <ActionCard item={item} testId={`opener-${i}`} key={i} />
          ))}
        </section>
      )}
      {ideas.length > 0 && (
        <section className="perception-block" data-testid="block-ideas">
          <h4>内容点子（{ideas.length}）</h4>
          {ideas.map((item, i) => (
            <ActionCard item={item} testId={`idea-${i}`} key={i} />
          ))}
        </section>
      )}
      {hypotheses.length > 0 && (
        <section className="perception-block" data-testid="block-hypotheses">
          <h4>假说（{hypotheses.length}·均未经确证）</h4>
          <ul className="perception-list">
            {hypotheses.map((x, i) => (
              <li key={i}>{x}</li>
            ))}
          </ul>
        </section>
      )}
      {enrichments.length > 0 && (
        <section className="perception-block" data-testid="block-enrich">
          <h4>待补证（{enrichments.length}）</h4>
          <div className="chips">
            {enrichments.map((x, i) => (
              <span className="chip muted-chip" key={i}>
                {x}
              </span>
            ))}
          </div>
        </section>
      )}
      {cautions.length > 0 && (
        <section className="perception-block" data-testid="block-cautions">
          <h4>AI 自陈注意（{cautions.length}）</h4>
          <ul className="perception-list muted">
            {cautions.map((x, i) => (
              <li key={i}>{x}</li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
export function ViewerTree({ roomId, vid }: { roomId: string; vid: string }) {
  const tree = useQuery({
    queryKey: ["tree", roomId, vid],
    queryFn: () => api.viewerTree(roomId, vid),
  });

  if (tree.isLoading) return <div className="state-loading">载入个人树…</div>;
  if (tree.isError)
    return (
      <div className="notice">
        {String(tree.error instanceof Error ? tree.error.message : tree.error)}
      </div>
    );
  const data = tree.data;
  if (!data) {
    // 同 Streamer：pending-not-fetching 静水变体的显式空态（运行期不可达）。
    return <div className="state-loading">载入个人树…</div>;
  }
  // 200 面服务端拼装恒带 viewer（缺档直接 404 先行），不再 ?? {} 防御。
  const viewer = data.viewer;
  const ai = data.ai;
  const aiStale = data.ai_stale ?? null;
  const episodes: EpisodeRow[] = Array.isArray(data.episodes) ? data.episodes : [];
  const mentions: MentionRow[] = Array.isArray(data.mentions) ? data.mentions : [];
  const viewerName = viewer.viewer?.name ?? viewer.profile?.name ?? null;

  const spansFor = (episodeId: string, fieldPath: string): MentionRow[] =>
    mentions.filter((m) => m.episode_id === episodeId && m.field_path === fieldPath);

  return (
    <section className="section">
      <div className="section-title">
        <h2>舰长态势 · {viewerName ?? vid}</h2>
        <a href={`#/viewers/${encodeURIComponent(vid)}/graph`}>局部图 →</a>
      </div>

      <div className="card viewer-head">
        {/* 卡头头像同走统一面（58px 默认档；旧实现裸 <img> 无 no-referrer + 空 div 占位）。 */}
        <Avatar face={viewer.profile?.face ?? null} name={viewerName} />
        <div>
          <div className="badges">
            <span className="badge fact">uid {vid}</span>
            <span className="badge">采集于 {fmtTime(viewer.collected_at)}</span>
            <span className={`badge ${ai?.status === "complete" ? "state" : ""}`}>
              Perception {ai?.status ?? "未运行"}
            </span>
            {/* 时效位：旧 AI 结论保留作参考但信源已翻 → 感知区块亮标不重删。 */}
            {aiStale === true && <AiStaleBadge testId="ai-stale-badge-tree" />}
          </div>
        </div>
      </div>

      {/* 感知结构化块（原裸 <p> 散文糊位）：有物才出场，无物静默。 */}
      {ai?.analysis ? <AiPerceptionCard analysis={ai.analysis} /> : null}

      <div className="section card">
        <h3>Episode 时间线（{episodes.length}）</h3>
        {episodes.length === 0 && (
          // 空轴不再 dead-end——附单查引导按钮。
          <div className="empty">
            尚无 Episode ——
            <SingleViewerRunButton vid={vid} />
          </div>
        )}
        <div className="timeline">
          {episodes.map((episode) => (
            <div className="timeline-item" key={episode.episode_id}>
              <div className="badges">
                <span className="badge fact">{episode.source ?? "?"}</span>
                <span className="badge">{episode.event_type ?? "?"}</span>
                {/* 时间戳语义徽标：平台给了 published_at 就用行为时刻（发布于）；
                    缺/空回落到 observed_at（采集于=我们看到这条的时刻），恒落其一不猜。 */}
                {episode.published_at && episode.published_at.trim() !== "" ? (
                  <span
                    className="badge episode-ts"
                    data-testid={`episode-ts-${episode.episode_id}`}
                    title="发布于=平台显示的行为时刻"
                  >
                    发布于 {fmtTime(episode.published_at)}
                  </span>
                ) : (
                  <span
                    className="badge episode-ts"
                    data-testid={`episode-ts-${episode.episode_id}`}
                    title="采集于=我们看到这条的时刻（非行为时刻）"
                  >
                    采集于 {fmtTime(episode.observed_at)}
                  </span>
                )}
              </div>
              {episode.title && (
                <div>
                  {episode.url ? (
                    <a href={String(episode.url)} target="_blank" rel="noreferrer">
                      {episode.title}
                    </a>
                  ) : (
                    episode.title
                  )}
                </div>
              )}
              {(episode.fields ?? []).map((field) => (
                <div className="field" key={field.path}>
                  <div className="small muted">{field.path}</div>
                  <MentionText text={field.text} spans={spansFor(episode.episode_id, field.path)} />
                </div>
              ))}
            </div>
          ))}
        </div>
      </div>

      <details className="card">
        <summary>原始 JSON（viewer + ai 缓存）</summary>
        <pre className="protocol">{JSON.stringify({ viewer, ai }, null, 2)}</pre>
      </details>
    </section>
  );
}
