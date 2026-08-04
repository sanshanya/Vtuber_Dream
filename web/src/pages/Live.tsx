/**
 * 直播数据页（Z4）：默认对比「最后一场 vs 上周平均值」，其下场次档案表。
 *
 * 数据面 = overview.live（shared/live_records.json 整场记录原样透传，B站
 * xlive/web-room/v1/record/getList 回放列表）。对比窗口口径：以最后一场开播时刻
 * 为锚、往前 7 天（含）内的其他场次取简单算术平均；样本 0 → 不臆造，显式空态。
 * 指标只呈现记录中真实存在的数值字段（时长恒由 start/end 差推导）——B站返回
 * 形状随账号/版本漂移，本页对所有可选字段做 presence 判别，不补齐不存在的数字。
 * FE-F2：
 * - status 汉化三态（ok→正常 / error→接口故障 / 其他或缺省→—）；
 * - error 分支独立错文并透传 errors 数组（不再撞「主播暂无回放列表」空态）；
 * - 场次表落地「观看/弹幕/在线」三列——仅当本场记录真实携带该字段才渲值，无则 —。
 */
import { useQuery } from "@tanstack/react-query";

import { api } from "../api";
import { KindRunButton } from "../components/KindRunButton";

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
  /** FE-F2：指标列——null = 本场记录未携带该字段（渲 —，不补齐）。 */
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

/** FE-F2/R1#2：status 汉化三态——ok→正常 / error→接口故障 / 其他或缺省→—。 */
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
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId),
  });
  if (overview.isLoading) return <div className="empty">载入直播数据…</div>;
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
  const rows = Array.isArray(live?.records) ? (live.records as Array<Record<string, unknown>>) : [];
  const records = rows.map(toRecord).sort((a, b) => (b.start ?? 0) - (a.start ?? 0));
  // FE-F2/R1#4：errors 数组原样透传。api.ts 的 OverviewView（F3 收口面）未把 errors
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
      <section className="section card">
        <div className="section-title">
          <h2>直播数据</h2>
          <span className="badge" data-testid="live-status">
            {live === null
              ? "档案面未建立"
              : `${liveStatusLabel(live.status)} · ${String(live.count ?? rows.length)} 场`}
          </span>
        </div>
        {/* Z4d 动作落页：本页数据 = 主播采集（回放列表 + profile/投稿）产物，钮住本页。 */}
        <div className="action-bar" data-testid="live-actions">
          <KindRunButton
            kind="collect_streamer"
            note="事实层：重抓主播 profile/投稿/直播回放（本页数据源）。舰长 AI 结论不受影响（保留为参考，信源有变会被标注）。"
          />
        </div>
        {/* FE-F2/R1#4：error 与 empty 分家——接口故障要透传 errors，不混进空态句式。 */}
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
                        // hdslb 图片防盗链：必须 referrerPolicy="no-referrer"。
                        <img
                          src={r.cover}
                          alt=""
                          className="live-cover"
                          referrerPolicy="no-referrer"
                          loading="lazy"
                        />
                      ) : (
                        "—"
                      )}
                    </td>
                    <td>{r.title}</td>
                    <td>{r.start !== null ? fmtClock(r.start) : "—"}</td>
                    <td>{fmtDuration(r.durationMin)}</td>
                    <td>{r.area || "—"}</td>
                    {/* FE-F2：指标列 presence 判别——本场记录真实携带才渲值，无则 —。 */}
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
