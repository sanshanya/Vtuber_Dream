/**
 * 直播数据页（Z4）：默认对比「最后一场 vs 上周平均值」，其下场次档案表。
 *
 * 数据面 = overview.live（shared/live_records.json 整场记录原样透传，B站
 * xlive/web-room/v1/record/getList 回放列表）。对比窗口口径：以最后一场开播时刻
 * 为锚、往前 7 天（含）内的其他场次取简单算术平均；样本 0 → 不臆造，显式空态。
 * 指标只呈现记录中真实存在的数值字段（时长恒由 start/end 差推导）——B站返回
 * 形状随账号/版本漂移，本页对所有可选字段做 presence 判别，不补齐不存在的数字。
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
  extras: Array<[string, number]>;
}

/** B站记录可选数值字段候选（presence 判别后展示，键由实测回放面决定）。 */
const EXTRA_NUM_KEYS: Array<[string, string]> = [
  ["online", "在线峰值"],
  ["watch_num", "观看人数"],
  ["danmu_num", "弹幕数"],
  ["live_time", "直播时长(秒)"],
];

function toRecord(row: Record<string, unknown>): LiveRecord {
  const start = num(row.start_time);
  const end = num(row.end_time);
  const durationMin =
    start !== null && end !== null && end >= start ? Math.round((end - start) / 60) : null;
  const extras: Array<[string, number]> = [];
  for (const [key, label] of EXTRA_NUM_KEYS) {
    const value = num(row[key]);
    if (value !== null) extras.push([label, value]);
  }
  return {
    title: text(row.title) || "（无标题）",
    start,
    durationMin,
    area: text(row.area_name) || text(row.parent_area_name),
    cover: text(row.cover),
    extras,
  };
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
  const live = overview.data.live ?? null;
  const rows = Array.isArray(live?.records) ? (live.records as Array<Record<string, unknown>>) : [];
  const records = rows.map(toRecord).sort((a, b) => (b.start ?? 0) - (a.start ?? 0));
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
          <span className="badge">
            {live === null
              ? "档案面未建立"
              : `${String(live.status)} · ${String(live.count ?? rows.length)} 场`}
          </span>
        </div>
        {/* Z4d 动作落页：本页数据 = 主播采集（回放列表 + profile/投稿）产物，钮住本页。 */}
        <div className="action-bar" data-testid="live-actions">
          <KindRunButton
            kind="collect_streamer"
            note="事实层：重抓主播 profile/投稿/直播回放（本页数据源）。注意：采集器会重建整个采集面并清空舰长 AI 缓存（历史照旧归档）。"
          />
        </div>
        {records.length === 0 ? (
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
