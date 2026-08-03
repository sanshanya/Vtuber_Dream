import { useQuery } from "@tanstack/react-query";

import { api } from "../api";
import { DeltaBlock } from "../components/DeltaBlock";
import { LeadsBlock } from "../components/LeadsBlock";
import { RunButton } from "../components/RunButton";
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
    const message = String(overview.error instanceof Error ? overview.error.message : overview.error);
    const missing = message.includes("collection");
    return (
      <section className="section card">
        <h2>房间面板</h2>
        <div className="notice">
          {missing ? "还没有采集数据——先触发一次运行：" : message}{" "}
          {missing && <RunButton />}
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
          <RunButton />
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
          <Situ analysis={situation.analysis} />
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

/**
 * situation.analysis 直渲（键与 demo.rs / AudienceSituationSubmission 同源：
 * executive_summary / interest_graph[entity,status,confidence,angles] /
 * situations[title,status,description] / content_opportunities[title,entity,format]）。
 */
function Situ({ analysis }: { analysis: any }) {
  const interests = Array.isArray(analysis.interest_graph) ? analysis.interest_graph : [];
  const situations = Array.isArray(analysis.situations) ? analysis.situations : [];
  const opportunities = Array.isArray(analysis.content_opportunities)
    ? analysis.content_opportunities
    : [];
  const calendars = Array.isArray(analysis.content_calendar) ? analysis.content_calendar : [];
  return (
    <>
      {analysis.executive_summary && <p>{String(analysis.executive_summary)}</p>}
      <div className="badges">
        <span className="badge state">兴趣实体 {interests.length}</span>
        <span className="badge ai">态势项 {situations.length}</span>
        <span className="badge action">内容机会 {opportunities.length}</span>
        <span className="badge fact">排期 {calendars.length}</span>
      </div>
      {interests.map((item: any, i: number) => (
        <span className="badge ai" key={`g${i}`} title={item.evidence_summary ?? ""}>
          {item.entity ?? "?"}
          {item.status ? ` · ${item.status}` : ""}
        </span>
      ))}
      {situations.length > 0 && (
        <ul className="delta-list">
          {situations.map((item: any, i: number) => (
            <li key={`s${i}`}>
              <strong>{item.title ?? "?"}</strong>
              <span className="badge state" style={{ margin: "0 6px" }}>
                {item.status ?? "?"}
              </span>
              <span className="muted">{item.description ?? ""}</span>
            </li>
          ))}
        </ul>
      )}
      {opportunities.length > 0 && (
        <ul className="delta-list">
          {opportunities.map((item: any, i: number) => (
            <li key={`c${i}`}>
              <strong>{item.title ?? item.entity ?? "?"}</strong>
              {item.format ? <span className="badge action">{item.format}</span> : null}{" "}
              <span className="muted">{item.why_now ?? ""}</span>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
