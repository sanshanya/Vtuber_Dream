import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { api, type ViewerRow } from "../api";
import { AiStaleBadge } from "../components/AiStaleBadge";
import { Avatar } from "../components/Avatar";
import { EmptyPoolHint } from "../components/EmptyPoolHint";
import { KindRunButton } from "../components/KindRunButton";
import { useRunTracker } from "../components/RunTracker";
import { errText, fmtTime } from "../format";
import { useStartRun } from "../hooks/useStartRun";

export function Viewers({ roomId }: { roomId: string }) {
  const viewers = useQuery({ queryKey: ["viewers", roomId], queryFn: () => api.viewers(roomId) });
  const tracker = useRunTracker();
  // 单查 run 走 useStartRun 登记进全局 RunTracker——hero 徽标轮询 +
  // 终态全失效兑现「完成后列表自动刷新」；409 错文在飞 id 由 hook 转跟随。
  const { start, error: singleError, followedId } = useStartRun();
  const [singleUid, setSingleUid] = useState("");
  const [pending, setPending] = useState(false);
  const [submittedRun, setSubmittedRun] = useState<string | null>(null);

  async function submitSingle() {
    setPending(true);
    try {
      const runId = await start({ kind: "viewer", viewer_uid: singleUid.trim() });
      if (runId !== null) setSubmittedRun(runId);
    } finally {
      setPending(false);
    }
  }

  if (viewers.isLoading) {
    return <div className="state-loading">载入观众列表…</div>;
  }
  if (viewers.isError) {
    return <div className="notice">观众列表加载失败：{errText(viewers.error)}</div>;
  }
  const rows = viewers.data ?? [];

  return (
    <section className="section">
      <div className="section-title">
        <h2>舰长列表</h2>
      </div>
      {/* 动作落页：名单与每人近态 = collect_guards 产物；逐舰长感知 = ai_viewers
          产物——钮住本页。重采会重建采集面并清空 AI 缓存（历史归档），文案直陈。 */}
      <div className="action-bar" data-testid="guard-actions">
        <KindRunButton
          kind="collect_guards"
          note="事实层：重拉大航海名单 + 每人动态/勋章/直播状态。旧 AI 结论保留作参考——信源有变的行会亮「信源已更新·待重判」。"
        />
        <KindRunButton
          kind="ai_viewers"
          note="认知层：对现有名单逐舰长跑感知（哈希失效驱动——信源未变的零成本复用，变了的/未跑的重判；灭灯按行点火）。"
        />
      </div>
      {rows.length === 0 ? (
        <EmptyPoolHint
          uid={singleUid}
          pending={pending}
          runActive={tracker.active}
          onUidChange={setSingleUid}
          onSubmit={() => void submitSingle()}
        />
      ) : (
        <ViewerTable rows={rows} />
      )}
      {/* 拒单错是错误面——badge danger；.notice 只留空池引导等非错提示。 */}
      {singleError && <span className="badge danger">单查提交被拒：{singleError}</span>}
      {followedId && (
        <p className="muted small">
          已有进行中的 run（{followedId.slice(0, 8)}…）——页头徽标转为跟随其进度。
        </p>
      )}
      {submittedRun && followedId === null && (
        <p className="muted small">
          已触发单查 run：<code>{submittedRun}</code>
          ——完成后列表自动刷新；进度与 events 流见页面顶部页头徽标。
        </p>
      )}
    </section>
  );
}

/** 大航海等级字面（B站 guard_level 语义：3=舰长 / 2=提督 / 1=总督）。 */
const GUARD_LABELS: Record<number, string> = { 1: "总督", 2: "提督", 3: "舰长" };

function ViewerTable({ rows }: { rows: ViewerRow[] }) {
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            <th>观众</th>
            <th>大航海</th>
            <th>勋章</th>
            <th>uid</th>
            <th>采集于</th>
            <th>Perception</th>
            {/* 舰长卡→关系卡：四微件整列。 */}
            <th>关系</th>
            <th>入口</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.uid}>
              {/* 身份列（大航海 API 一发即带 face/guard_level/medal_level——
                  旧版站的观众签名是「头像+名字」，有头像是人能认出人的前提）。 */}
              <td>
                <span className="viewer-cell">
                  <Avatar face={row.face} name={row.name} size="sm" />
                  <a href={`#/viewers/${encodeURIComponent(row.uid)}/tree`}>{row.name ?? "—"}</a>
                </span>
              </td>
              <td>
                {row.guard_level !== null ? (
                  <span className="badge state">
                    {GUARD_LABELS[row.guard_level] ?? `Lv${row.guard_level}`} {row.guard_level}
                  </span>
                ) : (
                  "—"
                )}
              </td>
              <td>{row.medal_level !== null ? `Lv${row.medal_level}` : "—"}</td>
              <td className="protocol">{row.uid}</td>
              <td>{fmtTime(row.collected_at)}</td>
              <td>
                <span className={`badge ${row.ai_completed ? "state" : ""}`}>
                  {row.ai_status ?? "未运行"}
                </span>
                {/* 时效位：旧结论保留作参考但信源已翻 → 亮标不重删（重跑才落新结论）。 */}
                {row.ai_stale === true && <AiStaleBadge testId="ai-stale-badge" />}
              </td>
              {/* 四微件——缺件落「未知」微行（设计钉同款狠法：无数据
                  是有信息的状态，不补文案、两态可辨）。身份一句盖 AI 徽标不入事实色。 */}
              <td data-testid={`relation-widgets-${row.uid}`}>
                <div className="micro-line">
                  {row.visit_count != null ? `第 ${row.visit_count} 次来` : <span className="muted small">第几次来：未知</span>}
                </div>
                <div className="micro-line">
                  {row.days_since_last != null ? `距上次 ${row.days_since_last} 天` : <span className="muted small">距上次：未知</span>}
                </div>
                <div className="micro-line" data-testid={`identity-line-${row.uid}`}>
                  {row.identity_line ? (
                    <>
                      <span className="badge ai">AI</span> {row.identity_line}
                    </>
                  ) : (
                    <span className="muted small">身份一句：未知</span>
                  )}
                </div>
                <div className="micro-line">
                  {row.latest_activity_date != null ? `最新动态 ${row.latest_activity_date}` : <span className="muted small">最新动态：未知</span>}
                </div>
              </td>
              <td>
                <a href={`#/viewers/${encodeURIComponent(row.uid)}/tree`}>舰长态势</a> ·{" "}
                <a href={`#/viewers/${encodeURIComponent(row.uid)}/graph`}>局部图</a>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
