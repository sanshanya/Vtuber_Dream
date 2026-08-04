import { useQuery } from "@tanstack/react-query";

import { api } from "../api";
import { AiStaleBadge } from "../components/AiStaleBadge";
import { MentionText, type MentionSpanLike } from "../components/MentionText";
import { SingleViewerRunButton } from "../components/SingleViewerRunButton";
import { fmtTime } from "../format";

interface MentionRow extends MentionSpanLike {
  episode_id?: string;
  field_path?: string;
  origin?: string;
  confidence?: number;
}

interface EpisodeRow {
  episode_id: string;
  source?: string;
  event_type?: string;
  observed_at?: string;
  published_at?: string;
  title?: string | null;
  url?: string | null;
  bvid?: string | null;
  fields?: Array<{ path: string; text: string; kind: string }>;
}

/** 个人树（D3）：viewer 卡 + ai 缓存卡 + Episode 时间线（mention 高亮）。 */
export function ViewerTree({ roomId, vid }: { roomId: string; vid: string }) {
  const tree = useQuery({
    queryKey: ["tree", roomId, vid],
    queryFn: () => api.viewerTree(roomId, vid),
  });

  if (tree.isLoading) return <div className="empty">载入个人树…</div>;
  if (tree.isError)
    return (
      <div className="notice">
        {String(tree.error instanceof Error ? tree.error.message : tree.error)}
      </div>
    );
  const data = tree.data ?? ({} as NonNullable<typeof tree.data>);
  const viewer = data.viewer ?? {};
  const ai = data.ai ?? null;
  const aiStale = data.ai_stale ?? null;
  const episodes: EpisodeRow[] = Array.isArray(data.episodes) ? data.episodes : [];
  const mentions: MentionRow[] = Array.isArray(data.mentions) ? data.mentions : [];

  const spansFor = (episodeId: string, fieldPath: string): MentionRow[] =>
    mentions.filter((m) => m.episode_id === episodeId && m.field_path === fieldPath);

  return (
    <section className="section">
      <div className="section-title">
        <h2>舰长态势 · {viewer.viewer?.name ?? viewer.profile?.name ?? vid}</h2>
        <a href={`#/viewers/${encodeURIComponent(vid)}/graph`}>局部图 →</a>
      </div>

      <div className="card viewer-head">
        {viewer.profile?.face ? <img className="avatar" src={String(viewer.profile.face)} alt="" /> : <div className="avatar" />}
        <div>
          <div className="badges">
            <span className="badge fact">uid {vid}</span>
            <span className="badge">采集于 {fmtTime(viewer.collected_at)}</span>
            <span className={`badge ${ai?.status === "complete" ? "state" : ""}`}>
              Perception {ai?.status ?? "未运行"}
            </span>
            {/* Z5c 时效位：旧 AI 结论保留作参考但信源已翻 → 感知区块亮标不重删。 */}
            {aiStale === true && <AiStaleBadge testId="ai-stale-badge-tree" />}
          </div>
          {ai?.analysis?.profile_summary && <p>{String(ai.analysis.profile_summary)}</p>}
        </div>
      </div>

      <div className="section card">
        <h3>Episode 时间线（{episodes.length}）</h3>
        {episodes.length === 0 && (
          // ag5-F7：空轴不再 dead-end——附单查引导按钮。
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
                <span className="badge">{fmtTime(episode.observed_at)}</span>
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
