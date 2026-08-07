/**
 * 主播介绍页（Z2 定稿首页）：主播卡（头像/签名/平台事实徽标）→ 复盘卡（首卡，
 * P1-1 裁决三）→ 制片人简报 → 运行概览（薄统计带 + 花费公示）→ 舰长栏。
 * 取代旧 Dashboard；R2 批5 D6：宏观态势直呈段整段退役（简报即态势展示面）。
 */
import { useQuery } from "@tanstack/react-query";

import { api, isApiError, type BudgetEstimate } from "../api";
import { AiStaleBadge } from "../components/AiStaleBadge";
import { BriefingCard, type EpisodeIndexEntry } from "../components/BriefingCard";
import { Avatar } from "../components/Avatar";
import { KindRunButton } from "../components/KindRunButton";
import { RunButton } from "../components/RunButton";
import { RecapCard } from "../components/RecapCard";
import { StreamerCard } from "../components/StreamerCard";
import { estimateCostCny, fmtCny, fmtInt, fmtTime, type UsageRow } from "../format";

/** D8 主钮旁预估行（/api/budget estimate 段渲染）：normal_cny 与 etd 双在场才出
 *  数字行；任一缺 → 「预估 —」（名册缺/空/未采集 = 服务端全 null 的同义面）。 */
function RunEstimateLine({ estimate }: { estimate: BudgetEstimate | null | undefined }) {
  const normal = estimate?.normal_cny ?? null;
  const etd = estimate?.etd_minutes ?? null;
  const lo = Array.isArray(etd) && typeof etd[0] === "number" ? etd[0] : null;
  const hi = Array.isArray(etd) && typeof etd[1] === "number" ? etd[1] : null;
  const ok =
    typeof normal === "number" &&
    Number.isFinite(normal) &&
    typeof lo === "number" &&
    typeof hi === "number" &&
    Number.isFinite(lo) &&
    Number.isFinite(hi);
  return (
    <span className="run-estimate muted small" data-testid="run-estimate">
      {ok
        ? `预估 ≈${fmtCny(normal)}（上限口径）· 约 ${lo}~${hi} 分钟`
        : "预估 —"}
    </span>
  );
}

/** D8「分层跑」次级菜单：ai_audience 从平铺位移入本菜单，引导文案原样保留
 *  （采集动作的钮在各自页侧——哪个页面数据由哪个动作产出，菜单只做导航）。 */
function TieredRunsMenu() {
  return (
    <details className="tiered-runs" data-testid="tiered-runs">
      <summary>分层跑（采集 / AI 分 kind，按需补跑）</summary>
      <div className="tiered-runs-body">
        <KindRunButton
          kind="ai_audience"
          note="认知层：不复采。基于现有各舰长感知（幂等缓存，已完成的自动复用）重推整体态势与行动建议——舰长感知齐时秒级收。"
        />
        <p className="muted small tiered-guide">
          主播采集 → <a href="#/live">直播数据页</a>（重抓 profile/投稿/回放，本页数据源）
        </p>
        <p className="muted small tiered-guide">
          舰长采集 → <a href="#/viewers">舰长列表页</a>（重拉大航海名单 + 每人近态）
        </p>
        <p className="muted small tiered-guide">
          舰长 AI 分析 → <a href="#/viewers">舰长列表页</a>（逐舰长感知，哈希失效驱动）
        </p>
      </div>
    </details>
  );
}

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
  // D8：主钮旁预估行——/api/budget 的 estimate 段（RunTracker 终态失效会连它一起刷新）。
  // 失败/空态不走错误面：无预估也只是「预估 —」。
  const budget = useQuery({
    queryKey: ["budget"],
    queryFn: () => api.budget(),
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
          <RunEstimateLine estimate={budget.data?.estimate ?? null} />
          <TieredRunsMenu />
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

  return (
    <>
      <StreamerCard
        profile={data.streamer ?? null}
        streamerUid={String(data.streamer_uid ?? "")}
        roomId={String(data.room_id ?? "")}
      />

      {/* v2 P1-1（裁决三）：首卡恒为复盘卡（有无数据均占首位）——疲惫态下播 5
          分钟要答案不要诊断；未生成/空场/就绪三态各有具名呈现。 */}
      <RecapCard recap={data.recap ?? null} />

      {/* Z5/C1（终裁 P0-5）：制片人简报居次——「发生了什么/该干嘛/该信谁」；
          未生成/沉默两态同样具名呈现（缺席必可见）。 */}
      <BriefingCard
        analysis={analysis}
        situationStatus={situation.status}
        aiCompletedAt={ai.completed_at ?? null}
        stale={(viewers.data ?? []).some((row) => row.ai_stale === true)}
        episodeIndex={(data.episode_index ?? {}) as Record<string, EpisodeIndexEntry>}
        nameOf={new Map((viewers.data ?? []).map((row) => [row.uid, row.name ?? row.uid]))}
      />

      {/* Z4d 动作落页：本页住「全量感知」（敏感谨慎钮，双段确认）与「分层跑」次级菜单
          （D8：主播 AI 分析从平铺位移入菜单；采集面动作仍在各自页侧——主播采集在
          「直播数据」页、舰长采集与舰长 AI 在「舰长列表」页，菜单内只做引导）。 */}
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
          <RunEstimateLine estimate={budget.data?.estimate ?? null} />
          <TieredRunsMenu />
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

      {/* R2 批5 D6（裁决）：「态势项」胶囊与宏观折叠组整段退役——简报（front_brief）
          即态势的展示面与承接口，矛盾数字（0 态势项 vs 满格简报）不可再同框。
          situation 字段本身保留（BriefingCard 的 front_brief 数据源，API deprecated）。 */}
    </>
  );
}
