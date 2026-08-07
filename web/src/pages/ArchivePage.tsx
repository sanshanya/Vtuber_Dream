import { Fragment } from "react";
import { useQuery } from "@tanstack/react-query";

import { api } from "../api";

/**
 * 存档页：存活天数 + 周健康四数 + 里程碑日历。
 * 纯事实派生面（铁律#3：零 AI）——数字与文案全部来自服务端
 * crates/live-server/src/app/archive.rs 的派生计算，前端只做 presence
 * 判别与「未就位」措辞兜底，绝不补齐不存在的数字。
 * T1 三态纪律：state-loading（呼吸中）/ notice（错态）/ empty（整面无已知事实）。
 */
export function ArchivePage() {
  const archive = useQuery({ queryKey: ["archive"], queryFn: api.archive });

  if (archive.isLoading) return <div className="state-loading">载入存档…</div>;
  if (archive.isError)
    return (
      <div className="notice">
        {String(archive.error instanceof Error ? archive.error.message : archive.error)}
      </div>
    );

  const view = archive.data;
  const healthRows = view?.weekly_health ?? [];
  // 空态判准：无存活锚点且周健康无任何已知行 = 事实面还没生成（跑一轮 collect 才有）。
  // 混态（锚缺失但周健康已知）不得吞已知行——未知段各自落「未就位/缺乏起始锚点」。
  if (!view || (view.alive_days === null && healthRows.every((row) => !row.known))) {
    return (
      <div className="empty" data-testid="archive-empty">
        存档面尚无事实数据（跑完至少一轮 collect 后生成）
      </div>
    );
  }

  const aliveSince = view.alive_since ? String(view.alive_since).slice(0, 10) : null;

  return (
    <section className="section">
      <div className="section-title">
        <h2>存档</h2>
        {/* 头注：存活起点口径必须留痕（锚 = 四种既存工件任一的最早时间），
            免得数字日后被当成凭空而来。 */}
        <span className="muted small" data-testid="archive-head-note">
          存活起点 = 最早可得的既存锚（历史快照 / 图库最早场次 / 直播档案任一的最早时间）
        </span>
      </div>

      <div className="card">
        <h3>存活天数</h3>
        {view.alive_days === null ? (
          // 铁律#3：无锚点不臆造天数——显式「缺乏起始锚点」。
          <div className="empty" data-testid="archive-alive-missing">
            存活 —（缺乏起始锚点）
          </div>
        ) : (
          <dl className="kv" data-testid="archive-alive">
            <dt>存活天数</dt>
            <dd>
              <span className="archive-alive-days">{view.alive_days}</span> 天
              {aliveSince && (
                <span className="muted small" style={{ marginLeft: 8 }}>
                  起始于 {aliveSince}
                </span>
              )}
            </dd>
          </dl>
        )}
      </div>

      <div className="card">
        <h3>周健康</h3>
        <dl className="kv archive-health" data-testid="archive-health">
          {healthRows.map((row) => (
            <Fragment key={row.key}>
              <dt>{row.label}</dt>
              <dd
                className={row.known ? undefined : "muted"}
                data-testid={`health-${row.key}`}
              >
                {row.value_text}
              </dd>
            </Fragment>
          ))}
        </dl>
      </div>

      <div className="card">
        <h3>里程碑日历</h3>
        <ul className="archive-milestones" data-testid="archive-milestones">
          {view.milestones.map((milestone) => (
            <li
              key={milestone.key}
              className={`archive-milestone ${milestone.state}`}
              data-testid={`milestone-${milestone.key}`}
              data-state={milestone.state}
            >
              <strong>{milestone.label}</strong>
              <span>{milestone.detail_text}</span>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}