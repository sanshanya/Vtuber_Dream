/**
 * 直播数据页：默认对比「最后一场 vs 上周平均值」，其下场次档案表。
 *
 * 数据面 = overview.live（shared/live_records.json 整场记录原样透传，B站
 * xlive/web-room/v1/record/getList 回放列表）。对比窗口口径：以最后一场开播时刻
 * 为锚、往前 7 天（含）内的其他场次取简单算术平均；样本 0 → 不臆造，显式空态。
 * 指标只呈现记录中真实存在的数值字段（时长恒由 start/end 差推导）——B站返回
 * 形状随账号/版本漂移，本页对所有可选字段做 presence 判别，不补齐不存在的数字。
 * 追加钉面：
 * - status 汉化三态（ok→正常 / error→接口故障 / 其他或缺省→—）；
 * - error 分支独立错文并透传 errors 数组（不再撞「主播暂无回放列表」空态）；
 * - 场次表落地「观看/弹幕/在线」三列——仅当本场记录真实携带该字段才渲值，无则 —。
 */
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../api";
import { KindRunButton } from "../components/KindRunButton";
import { NoReferrerImg } from "../components/NoReferrerImg";

const WEEK_SECONDS = 7 * 24 * 3600;

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function num(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

interface LiveRecord {
  title: string;
  start: number | null;
  durationMin: number | null;
  area: string;
  cover: string;
  /** 指标列——null = 本场记录未携带该字段（渲 —，不补齐）。 */
  watchNum: number | null;
  danmuNum: number | null;
  online: number | null;
}

function toRecord(row: Record<string, unknown>): LiveRecord {
  const start = num(row.start_time);
  const end = num(row.end_time);
  const durationMin =
    start !== null && end !== null && end >= start ? Math.round((end - start) / 60) : null;
  return {
    title: text(row.title) || "（无标题）",
    start,
    durationMin,
    area: text(row.area_name) || text(row.parent_area_name),
    cover: text(row.cover),
    watchNum: num(row.watch_num),
    danmuNum: num(row.danmu_num),
    online: num(row.online),
  };
}

/** status 汉化三态——ok→正常 / error→接口故障 / 其他或缺省→—。 */
const LIVE_STATUS_LABELS: Record<string, string> = {
  ok: "正常",
  error: "接口故障",
};

function liveStatusLabel(status: unknown): string {
  return typeof status === "string" ? (LIVE_STATUS_LABELS[status] ?? "—") : "—";
}

function fmtClock(unix: number): string {
  const d = new Date(unix * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fmtDuration(minutes: number | null): string {
  if (minutes === null) return "—";
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return h > 0 ? `${h} 小时 ${m} 分` : `${m} 分`;
}

export function Live({ roomId }: { roomId: string }) {
  const queryClient = useQueryClient();
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId),
  });
  if (overview.isLoading) return <div className="state-loading">载入直播数据…</div>;
  if (overview.isError) {
    return (
      <section className="section card">
        <h2>直播数据</h2>
        <div className="notice">{String(overview.error)}</div>
      </section>
    );
  }
  // F3：overview 接口收紧后 pending 变体 data 类型含 undefined（F3 单宽限；他单重做消费面时收回）。
  const live = overview.data?.live ?? null;
  // 采录场次窗（WS 实采）——与 B站回放列表是两个独立事实源（互不替补，同页分卡）。
  const wsWindowsDoc = overview.data?.ws_windows ?? null;
  const wsWindows = Array.isArray(wsWindowsDoc?.windows) ? wsWindowsDoc!.windows! : [];
  const fmtYuan = (v: unknown): string =>
    typeof v === "number" && Number.isFinite(v) ? `¥${v.toFixed(1)}` : "—";
  const fmtIntOrDash = (v: unknown): string =>
    typeof v === "number" && Number.isFinite(v) ? String(v) : "—";
  const fmtWindowClock = (unix: unknown): string =>
    typeof unix === "number" && Number.isFinite(unix) ? fmtClock(unix) : "—";
  const rows = Array.isArray(live?.records) ? (live.records as Array<Record<string, unknown>>) : [];
  const records = rows.map(toRecord).sort((a, b) => (b.start ?? 0) - (a.start ?? 0));
  // errors 数组原样透传。api.ts 的 OverviewView（收口面）未把 errors
  // 写进窄形状——这里是 presence 护栏读（服务端 live_records.json 本就带 errors 键）；
  // 形状漂移时非字符串项丢弃，绝不臆造。
  const liveErrorsRaw = live === null ? undefined : (live as { errors?: unknown }).errors;
  const liveErrors: string[] = Array.isArray(liveErrorsRaw)
    ? liveErrorsRaw.filter((e): e is string => typeof e === "string" && e.length > 0)
    : [];
  const last = records[0];
  const weekPool =
    last?.start !== null && last !== undefined
      ? records.filter(
          (r) =>
            r.start !== null && r.start < last.start! && r.start >= last.start! - WEEK_SECONDS,
        )
      : [];
  const avg = <T,>(items: T[], pick: (r: T) => number | null): number | null => {
    const values = items.map(pick).filter((v): v is number => v !== null);
    if (values.length === 0) return null;
    return values.reduce((a, b) => a + b, 0) / values.length;
  };
  const weekAvgDuration = avg(weekPool, (r) => r.durationMin);

  return (
    <>
      {/* 采录场次窗（WS 实采的第一事实源）——B站回放列表空≠没直播过；
          本卡事实 = ws-replay 落盘的 ai/ws_windows.json（一场一窗）。 */}
      <section className="section card" data-testid="ws-windows-card">
        <div className="section-title">
          <h2>采录场次窗（WS 实采）</h2>
          <span className="muted small">
            {wsWindows.length > 0 ? `${wsWindows.length} 场` : ""}
          </span>
        </div>
        {wsWindows.length === 0 ? (
          <div className="empty" data-testid="ws-windows-empty">
            尚无采录场次——跑一次 ws-replay 后此处呈场（一场一窗：窗时刻/发言人数/弹幕 /SC/付费礼
            物额/上舰播报）。
          </div>
        ) : (
          <table className="data-table" data-testid="ws-windows-table">
            <thead>
              <tr>
                <th>窗</th>
                <th>发言人数</th>
                <th>弹幕</th>
                <th>SC</th>
                <th>付费礼物</th>
                <th>上舰/播报</th>
              </tr>
            </thead>
            <tbody>
              {wsWindows.map((w, i) => (
                <tr key={w.session?.rid ?? i}>
                  <td>
                    {fmtWindowClock(w.session?.start_timestamp)} → {fmtWindowClock(w.session?.end_timestamp)}
                  </td>
                  <td data-testid={`ws-window-speakers-${i}`}>{fmtIntOrDash(w.speakers)}</td>
                  <td>{fmtIntOrDash(w.danmaku)}</td>
                  <td>{fmtIntOrDash(w.super_chat)}</td>
                  <td>
                    {w.money && (w.money.paid_gifts ?? 0) > 0
                      ? `${fmtYuan(w.money.gift_yuan)}（${fmtIntOrDash(w.money.paid_gifts)} 次）`
                      : "零付费礼物"}
                  </td>
                  <td>
                    {fmtIntOrDash(w.money?.guard_buys)} / {fmtIntOrDash(w.money?.toasts)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
      <section className="section card">
        <div className="section-title">
          <h2>直播数据（B站回放接口）</h2>
          <span className="badge" data-testid="live-status">
            {live === null
              ? "档案面未建立"
              : `${liveStatusLabel(live.status)} · ${String(live.count ?? rows.length)} 场`}
          </span>
        </div>
        {/* 动作落页：本页数据 = 主播采集（回放列表 + profile/投稿）产物，钮住本页。 */}
        <div className="action-bar" data-testid="live-actions">
          <KindRunButton
            kind="collect_streamer"
            note="事实层：重抓主播 profile/投稿/直播回放（本页数据源）。舰长 AI 结论不受影响（保留为参考，信源有变会被标注）。"
          />
          {/* 自动采录开关（唯一写面 = POST /api/rooms/:uid/auto-collect）：
              OFF = 缺档/显 false；转动后哨兵收尾臂每次收播 fire 一场全量 run
              （kind=full；预算上限由 pipeline 自带保险丝管，本钮不再二道闸）。 */}
          <button
            className="kind-btn"
            data-testid="auto-collect-toggle"
            aria-pressed={overview.data?.auto_collect?.enabled === true}
            onClick={async () => {
              const next = overview.data?.auto_collect?.enabled !== true;
              await api.setAutoCollect(roomId, next);
              await queryClient.invalidateQueries({ queryKey: ["overview", roomId] });
            }}
          >
            自动采录：{overview.data?.auto_collect?.enabled === true ? "开" : "关"}
          </button>
          <span className="muted small" data-testid="auto-collect-hint">
            {overview.data?.auto_collect?.enabled === true
              ? "每次收播自动起一场全量采集+感知（本场次指纹仅一次）"
              : "收播只录不采——关了省 token，想看复盘再手动跑"}
          </span>
        </div>
        {/* error 与 empty 分家——接口故障要透传 errors，不混进空态句式。 */}
        {live?.status === "error" ? (
          <div className="notice" data-testid="live-error">
            回放接口故障——B站 record/getList 抓取失败
            {liveErrors.length > 0 ? `：${liveErrors.join("；")}` : ""}
            。每次主播采集自动重试；接口恢复后此处呈现场次卡与对比。
          </div>
        ) : records.length === 0 ? (
          <div className="empty" data-testid="live-empty">
            尚无直播场次记录——B站回放接口返回为空（主播暂无回放列表）。
            每次全量运行自动刷新本面；积累 ≥1 场后此处呈现场次卡，≥2 场后开启「最后一场 vs
            上周均值」对比。
          </div>
        ) : (
          <>
            <div className="grid stats">
              <div className="card stat" data-testid="live-last">
                <span>最后一场：{last.title}</span>
                <strong>{last.start !== null ? fmtClock(last.start) : "—"}</strong>
                <span>时长 {fmtDuration(last.durationMin)}</span>
              </div>
              <div className="card stat" data-testid="live-week-avg">
                <span>上周均值（{weekPool.length} 场样本）</span>
                <strong>{weekAvgDuration !== null ? fmtDuration(Math.round(weekAvgDuration)) : "—"}</strong>
                <span>对比窗口 = 最后一场开播前 7 天</span>
              </div>
            </div>
            {weekPool.length === 0 && (
              <p className="muted small">对比未开：最后一场之前 7 天内无其他场次档案。</p>
            )}
            <table className="data-table">
              <thead>
                <tr>
                  <th>封面</th>
                  <th>场次</th>
                  <th>开播时间</th>
                  <th>时长</th>
                  <th>分区</th>
                  <th>观看</th>
                  <th>弹幕</th>
                  <th>在线</th>
                </tr>
              </thead>
              <tbody>
                {records.map((r, i) => (
                  <tr key={i}>
                    <td>
                      {r.cover ? (
                        <NoReferrerImg src={r.cover} alt="" className="live-cover" />
                      ) : (
                        "—"
                      )}
                    </td>
                    <td>{r.title}</td>
                    <td>{r.start !== null ? fmtClock(r.start) : "—"}</td>
                    <td>{fmtDuration(r.durationMin)}</td>
                    <td>{r.area || "—"}</td>
                    {/* 指标列 presence 判别——本场记录真实携带才渲值，无则 —。 */}
                    <td>{r.watchNum ?? "—"}</td>
                    <td>{r.danmuNum ?? "—"}</td>
                    <td>{r.online ?? "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}
      </section>
    </>
  );
}
