import { useQuery } from "@tanstack/react-query";

import { api, type EpisodeRow, type MentionRow } from "../api";
import { AiStaleBadge } from "../components/AiStaleBadge";
import { Avatar } from "../components/Avatar";
import { MentionText } from "../components/MentionText";
import { SingleViewerRunButton } from "../components/SingleViewerRunButton";
import { fmtTime } from "../format";

/** 个人树（D3）：viewer 卡 + ai 缓存卡 + Episode 时间线（mention 高亮）。
 * 行型 EpisodeRow/MentionRow 的家在 api.ts（F3 收口：本文件只对真型消费，不再断言 any[]）。 */
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
        {/* R2#7：卡头头像同走统一面（58px 默认档；旧实现裸 <img> 无 no-referrer + 空 div 占位）。 */}
        <Avatar face={viewer.profile?.face ?? null} name={viewerName} />
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
          {ai?.analysis?.profile_summary ? <p>{String(ai.analysis.profile_summary)}</p> : null}
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
