/**
 * 线索账本页（Z3 定稿导航末位）：自旧 Dashboard 拆出，LeadsBlock 承接呈现。
 * G2-B 审批缝宿主：待审行「批准」钮（一击即飞）→ POST 审批缝；404/422 就地
 * danger；成功后 overview 查询失效重取。标题行 L1 自治位徽标读 overview
 * leads.autonomy（0=纯人工 / 1=L1 自动批准+预算消费）。
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { api } from "../api";
import { LeadsBlock } from "../components/LeadsBlock";

export function Leads({ roomId }: { roomId: string }) {
  const queryClient = useQueryClient();
  const [approveError, setApproveError] = useState<string | null>(null);
  const [busyLeadId, setBusyLeadId] = useState<string | null>(null);
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId),
  });
  const approve = useMutation({
    mutationFn: (leadId: string) => api.approveLead(roomId, leadId),
    onMutate: (leadId) => {
      setApproveError(null);
      setBusyLeadId(leadId);
    },
    onError: (error) => {
      setApproveError(String(error instanceof Error ? error.message : error));
      setBusyLeadId(null);
    },
    onSuccess: () => {
      setBusyLeadId(null);
      // 批准翻转账本 → overview（LeadsBlock 数据源）失效重取。
      void queryClient.invalidateQueries({ queryKey: ["overview", roomId] });
    },
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
  const autonomy = overview.data.leads?.autonomy === 1;
  return (
    <section className="section card">
      <div className="section-title">
        <h2>线索账本</h2>
        <span className="badge" data-testid="leads-autonomy" title="collection.leads_autonomy：0=纯人工审批；1=collect 尾段自动批准 creator/search 且目标不在册的 pending 线索，并按预算消费">
          自治 L1：{autonomy ? "开" : "关"}
        </span>
      </div>
      {approveError && (
        <span className="badge danger" data-testid="lead-approve-error">
          {approveError}
        </span>
      )}
      <LeadsBlock
        leads={overview.data.leads ?? {}}
        onApprove={(leadId) => approve.mutate(leadId)}
        busyLeadId={busyLeadId}
      />
    </section>
  );
}
