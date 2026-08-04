/**
 * 线索账本页（Z3 定稿导航末位）：自旧 Dashboard 拆出，LeadsBlock 原样承接。
 * 账本编辑面 = leads.jsonl（人工审批刻意只读，薄切设计维持）。
 */
import { useQuery } from "@tanstack/react-query";

import { api } from "../api";
import { LeadsBlock } from "../components/LeadsBlock";

export function Leads({ roomId }: { roomId: string }) {
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId),
  });
  if (overview.isLoading) return <div className="empty">载入线索账本…</div>;
  if (overview.isError) {
    return (
      <section className="section card">
        <h2>线索账本</h2>
        <div className="notice">{String(overview.error)}</div>
      </section>
    );
  }
  return (
    <section className="section card">
      <div className="section-title">
        <h2>线索账本</h2>
      </div>
      <LeadsBlock leads={overview.data.leads ?? {}} />
    </section>
  );
}
