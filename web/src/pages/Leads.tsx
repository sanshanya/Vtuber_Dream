/**
 * 线索账本页（定稿导航末位）：自旧 Dashboard 拆出，LeadsBlock 承接呈现。
 * 审批缝 + 拒绝缝宿主：待审行「批准/拒绝」钮（一击即飞）→
 * POST 审批/拒绝缝；404/422 就地 danger；成功后 overview 查询失效重取。
 * 拒绝携带单一 reason 自由文本（空合法 = 服务端 NULL 留档）；
 * 组级「全批」由 LeadsBlock 前端逐行 fan-out 到本页 mutation。
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { api } from "../api";
import { LeadsBlock } from "../components/LeadsBlock";
import { errText } from "../format";

export function Leads({ roomId }: { roomId: string }) {
  const queryClient = useQueryClient();
  const [approveError, setApproveError] = useState<string | null>(null);
  const [rejectError, setRejectError] = useState<string | null>(null);
  const [busyLeadId, setBusyLeadId] = useState<string | null>(null);
  const [busyRejectId, setBusyRejectId] = useState<string | null>(null);
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
      // errText 单家（收敛：不再手抄同形表达式）。
      setApproveError(errText(error));
      setBusyLeadId(null);
    },
    onSuccess: () => {
      setBusyLeadId(null);
      // 批准翻转账本 → overview（LeadsBlock 数据源）失效重取。
      void queryClient.invalidateQueries({ queryKey: ["overview", roomId] });
    },
  });
  // 拒绝缝宿主——拒因行内自由文本（空合法 = 服务端 NULL 留档）。
  const reject = useMutation({
    mutationFn: ({ leadId, reason }: { leadId: string; reason: string }) =>
      api.rejectLead(roomId, leadId, { reason }),
    onMutate: ({ leadId }) => {
      setRejectError(null);
      setBusyRejectId(leadId);
    },
    onError: (error) => {
      setRejectError(errText(error));
      setBusyRejectId(null);
    },
    onSuccess: () => {
      setBusyRejectId(null);
      void queryClient.invalidateQueries({ queryKey: ["overview", roomId] });
    },
  });
  if (overview.isLoading) return <div className="state-loading">载入线索账本…</div>;
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
      {approveError && (
        <span className="badge danger" data-testid="lead-approve-error">
          {approveError}
        </span>
      )}
      {rejectError && (
        <span className="badge danger" data-testid="lead-reject-error">
          {rejectError}
        </span>
      )}
      <LeadsBlock
        leads={overview.data?.leads ?? {}}
        onApprove={(leadId) => approve.mutate(leadId)}
        onReject={(leadId, reason) => reject.mutate({ leadId, reason })}
        busyLeadId={busyLeadId}
        busyRejectId={busyRejectId}
      />
    </section>
  );
}
