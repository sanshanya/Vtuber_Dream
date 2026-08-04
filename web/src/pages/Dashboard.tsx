import { useQuery } from "@tanstack/react-query";

import { api, isApiError } from "../api";
import { DeltaBlock } from "../components/DeltaBlock";
import { LeadsBlock } from "../components/LeadsBlock";
import { Situ } from "../components/Situ";
import { estimateCostCny, fmtCny, fmtInt, fmtTime, type UsageRow } from "../format";

/** 房间面板（D3）：collection 状态 + ai 状态 + delta 区块 + leads 区块 + token/费用公示。 */
export function Dashboard({ roomId }: { roomId: string }) {
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId),
  });

  if (overview.isLoading) {
    return <div className="empty">载入房间面板…</div>;
  }
  if (overview.isError) {
    const error: unknown = overview.error;
    // ag5-F6：空态判别走 ApiError.status（404 = 尚无采集快照），不再子串匹配文案。
    const missing = isApiError(error) && error.status === 404;
    const message = String(error instanceof Error ? error.message : error);
    return (
      <section className="section card">
        <h2>房间面板</h2>
        <div className="notice">
          {missing ? "还没有采集数据——用页面右上角的「触发全量感知」跑一轮：" : message}
        </div>
      </section>
    );
  }
  const data = overview.data;
  const collection = data.collection ?? {};
  const ai = data.ai ?? {};
  const situation = data.situation ?? {};
  const usage: UsageRow | undefined = ai.usage ?? undefined;
  const cost = estimateCostCny(usage);

  const stats: Array<[string, string]> = [
    ["采集状态", collection.status ?? "—"],
    ["采集时间", fmtTime(collection.finished_at ?? collection.started_at)],
    ["AI 状态", ai.status ?? "—"],
    ["AI 完成于", fmtTime(ai.completed_at)],
    ["输入 tokens", fmtInt(usage?.input_tokens)],
    ["输出 tokens", fmtInt(usage?.output_tokens)],
  ];

  return (
    <>
      <section className="section">
        <div className="section-title">
          <h2>房间面板</h2>
        </div>
        <div className="grid stats">
          {stats.map(([label, value]) => (
            <div className="card stat" key={label}>
              <span>{label}</span>
              <strong>{value}</strong>
            </div>
          ))}
        </div>
        {cost !== null && (
          <p className="muted small">
            最近运行估算花费：{fmtCny(cost)}（上限估算：按缓存未命中费率折算，见
            <code>src/constants.ts</code> 价目）· LLM 请求 {fmtInt(usage?.llm_requests)} 次 ·
            工具调用 {fmtInt(usage?.tool_calls)} 次
          </p>
        )}
      </section>

      <section className="section card">
        <div className="section-title">
          <h2>vs 上轮</h2>
        </div>
        <DeltaBlock delta={data.delta ?? { baseline_only: true }} />
      </section>

      <section className="section card">
        <div className="section-title">
          <h2>整体态势</h2>
        </div>
        {situation.status === "complete" && situation.analysis ? (
          // synthetic_demo 徽标的数据面在 collection/ai/situation 任一分段（demo.rs 写位随工件；
          // overview 原样透传读取文件，前端不臆造单一来源位）。
          <Situ
            analysis={situation.analysis}
            synthetic={
              collection.synthetic_demo === true ||
              ai.synthetic_demo === true ||
              situation.synthetic_demo === true
            }
          />
        ) : (
          <div className="empty">整体态势尚未形成（跑完 Audience 阶段后呈现）</div>
        )}
      </section>

      <section className="section card">
        <div className="section-title">
          <h2>线索账本</h2>
        </div>
        <LeadsBlock leads={data.leads ?? {}} />
      </section>
    </>
  );
}
