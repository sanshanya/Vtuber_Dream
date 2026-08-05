/**
 * 主播介绍页（Z2 定稿首页）：主播卡（头像/签名/平台事实徽标）→ 运行概览（薄统计带 +
 * 花费公示）→ 整体态势。取代旧 Dashboard——「vs 上轮」裁决为底层参考信号不上页面
 * （Z2 裁定），线索账本独立成 #/leads 页。
 */
import { useQuery } from "@tanstack/react-query";

import { api, isApiError } from "../api";
import { AiStaleBadge } from "../components/AiStaleBadge";
import { BriefingCard } from "../components/BriefingCard";
import { Avatar } from "../components/Avatar";
import { KindRunButton } from "../components/KindRunButton";
import { RunButton } from "../components/RunButton";
import { RecapCard } from "../components/RecapCard";
import { Situ } from "../components/Situ";
import { StreamerCard } from "../components/StreamerCard";
import { estimateCostCny, fmtCny, fmtInt, fmtTime, type UsageRow } from "../format";

export function Streamer({ roomId }: { roomId: string }) {
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId),
  });
  // 舰长栏（Z2b）：大航海名单是采集一发的产物，首页必须见人——
  // 概览成功后才拉名单（404 空态在 overview 分支先行处理）。
  const viewers = useQuery({
    queryKey: ["viewers", roomId],
    queryFn: () => api.viewers(roomId),
    enabled: overview.data !== undefined && !overview.isError,
  });

  if (overview.isLoading) {
    return <div className="state-loading">载入主播资料…</div>;
  }
  if (overview.isError) {
    const error: unknown = overview.error;
    // ag5-F6：空态判别走 ApiError.status（404 = 尚无采集快照），不再子串匹配文案。
    const missing = isApiError(error) && error.status === 404;
    const message = String(error instanceof Error ? error.message : error);
    return (
      <section className="section card">
        <h2>主播介绍</h2>
        <div className="notice">
          {missing
            ? "还没有采集数据——请先在下方「感知动作」栏跑一轮全量感知（或到「舰长列表」单查冷启动）。"
            : message}
        </div>
        {/* 空态也要给动作栏：首次冷启动的唯一入口正是本页。 */}
        <h3>感知动作</h3>
        <div className="action-bar" data-testid="room-actions">
          <RunButton viewerCount={null} />
        </div>
      </section>
    );
  }
  const data = overview.data;
  if (!data) {
    // react-query 类型面有 pending-not-fetching 变体（isLoading/isError 双守卫外的静水区；
    // 本查询无 enabled 门、运行期不可达）——显式落空态，不裸解引用。
    return <div className="state-loading">载入主播资料…</div>;
  }
  const collection = data.collection ?? {};
  const ai = data.ai ?? {};
  const situation = data.situation ?? {};
  const usage: UsageRow | undefined = ai.usage ?? undefined;
  const cost = estimateCostCny(usage);
  const stats: Record<string, unknown> | null =
    data.graph_stats && typeof data.graph_stats === "object"
      ? (data.graph_stats as Record<string, unknown>)
      : null;
  const statNum = (key: string): number | null =>
    stats && typeof stats[key] === "number" ? (stats[key] as number) : null;
  const analysis = situation.analysis as Record<string, unknown> | undefined;
  const situationCount =
    situation.status === "complete" && Array.isArray(analysis?.situations)
      ? (analysis?.situations as unknown[]).length
      : null;

  return (
    <>
      <StreamerCard
        profile={data.streamer ?? null}
        streamerUid={String(data.streamer_uid ?? "")}
        roomId={String(data.room_id ?? "")}
      />

      {/* Z5/C1（终裁 P0-5）：制片人简报 = 首屏第一卡——「发生了什么/该干嘛/该信谁」
          免滚动可答；未生成/沉默两态同样具名呈现（缺席必可见）。 */}
      <BriefingCard
        roomId={roomId}
        analysis={analysis}
        situationStatus={situation.status}
        aiCompletedAt={ai.completed_at ?? null}
        stale={(viewers.data ?? []).some((row) => row.ai_stale === true)}
      />

      {/* P0-2（迭代细则 v1 §1）：下播复盘卡——四纯规则数+AI命名件+未知行恒在。
          她下播 5 分钟的第一站；未生成/空场/就绪三态各有具名呈现。 */}
      <RecapCard recap={data.recap ?? null} />

      {/* Z4d 动作落页：本页住「全量感知」（敏感谨慎钮，双段确认）与「主播 AI 分析」
          （认知层聚合，不重采）。采集面动作：主播采集在「直播数据」页、舰长采集在
          「舰长列表」页——钮随身段（哪个页面消费哪个产物）。 */}
      <section className="section card" data-testid="room-actions">
        <div className="section-title">
          <h2>感知动作</h2>
          <span className="muted small">事实层采集 → 认知层 AI：一次全量，或分层补跑</span>
        </div>
        <div className="action-bar">
          <RunButton
            viewerCount={
              typeof collection.viewer_count === "number" ? collection.viewer_count : null
            }
          />
          <KindRunButton
            kind="ai_audience"
            note="认知层：不复采。基于现有各舰长感知（幂等缓存，已完成的自动复用）重推整体态势与行动建议——舰长感知齐时秒级收。"
          />
        </div>
      </section>

      {/* Z3：旧版站签名指标条（大图数）——感知引擎的存量自豪位。
          graph_stats 缺图态 → 「—」，不臆造数字。 */}
      <section className="section">
        <div className="grid stats static" data-testid="kpi-strip">
          <div className="card stat">
            <strong>{fmtInt(collection.viewer_count ?? null)}</strong>
            <span>舰长</span>
          </div>
          <div className="card stat">
            <strong>{fmtInt(statNum("episodes"))}</strong>
            <span>Episode事实</span>
          </div>
          <div className="card stat">
            <strong>{fmtInt(statNum("mentions"))}</strong>
            <span>精确Mention</span>
          </div>
          <div className="card stat">
            <strong>{fmtInt(statNum("entities"))}</strong>
            <span>动态实体</span>
          </div>
          <div className="card stat">
            <strong>{fmtInt(statNum("relations"))}</strong>
            <span>当前关系</span>
          </div>
          <div className="card stat">
            <strong>{fmtInt(situationCount)}</strong>
            <span>态势项</span>
          </div>
        </div>
      </section>

      <section className="section card">
        <div className="section-title">
          <h2>运行概览</h2>
        </div>
        <div className="badges overview-strip">
          <span className="badge" data-testid="run-collection">
            采集 {String(collection.status ?? "—")}
          </span>
          <span className="badge">
            采集于 {fmtTime(collection.finished_at ?? collection.started_at)}
          </span>
          <span className="badge" data-testid="run-ai">
            AI {String(ai.status ?? "—")}
          </span>
          <span className="badge">AI 完成于 {fmtTime(ai.completed_at)}</span>
          {cost !== null && <span className="badge action">估算花费 {fmtCny(cost)}</span>}
        </div>
        {cost !== null && (
          // R1#9：价目内联公示，不再指引跳源码（constants.ts TOKEN_RATES 同值真源）。
          <p className="muted small">
            花费为上限估算（按缓存未命中费率折算：输入 ¥2 / 输出 ¥8 每百万 token）· LLM 请求{" "}
            {fmtInt(usage?.llm_requests)} 次 · 工具调用 {fmtInt(usage?.tool_calls)} 次
          </p>
        )}
        {/* 旧版站四层语色 legend（分层哲学肉眼可见）。 */}
        <div className="legend">
          <span>
            <i className="dot fact"></i>平台事实
          </span>
          <span>
            <i className="dot ai"></i>AI语义
          </span>
          <span>
            <i className="dot state"></i>状态判断
          </span>
          <span>
            <i className="dot action"></i>行动建议
          </span>
        </div>
      </section>

      <section className="section card">
        <div className="section-title">
          <h2>舰长</h2>
          <a href="#/viewers">全部舰长 →</a>
        </div>
        {viewers.data && viewers.data.length > 0 ? (
          <div className="guard-strip" data-testid="guard-strip">
            {viewers.data.map((row) => (
              <a className="guard-chip" key={row.uid} href={`#/viewers/${encodeURIComponent(row.uid)}/tree`}>
                {/* R2#7：头像统一面走 Avatar（防盗链/首字 fallback 单源；strip 档 = xs 26px）。 */}
                <Avatar face={row.face} name={row.name} size="xs" />
                <strong>{row.name ?? row.uid}</strong>
                <span className={`badge${row.ai_completed ? " state" : ""}`}>
                  {row.ai_status ?? "—"}
                </span>
                {/* Z5c 时效位（strip 紧凑面）：与列表/tree 同源，复用 .badge.action。 */}
                {row.ai_stale === true && <AiStaleBadge testId="ai-stale-badge-strip" />}
              </a>
            ))}
          </div>
        ) : viewers.isLoading ? (
          <div className="state-loading">载入舰长…</div>
        ) : (
          <div className="empty">舰长名单为空——跑一轮全量感知或到「舰长列表」单查。</div>
        )}
      </section>

      {situation.status === "complete" && situation.analysis ? (
        // P0-2（迭代细则 v1 §1）：宏观态势段下沉为默认折叠——复盘卡承接她下播 5 分钟，
        // 宏观段只在想深挖时才展开（验收钉③：default-collapsed）。
        // synthetic_demo 徽标的数据面在 collection/ai/situation 任一分段（demo.rs 写位随工件；
        // overview 原样透传读取文件，前端不臆造单一来源位）。Z2b：各态势部件拆卡由 Situ 自持。
        <details className="section" data-testid="macro-details">
          <summary>
            整体态势（宏观）——{fmtInt(situationCount)} 项，点开看
          </summary>
          <Situ
            analysis={situation.analysis}
            synthetic={
              collection.synthetic_demo === true ||
              ai.synthetic_demo === true ||
              situation.synthetic_demo === true
            }
          />
        </details>
      ) : (
        <section className="section card">
          <h2>整体态势</h2>
          <div className="empty">整体态势尚未形成（跑完 Audience 阶段后呈现）</div>
        </section>
      )}
    </>
  );
}
