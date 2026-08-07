/**
 * 线索账本页（Z3 定稿导航末位）：自旧 Dashboard 拆出，LeadsBlock 承接呈现。
 * G2-B 审批缝 + D9/R2-批6 拒绝缝宿主：待审行「批准/拒绝」钮（一击即飞）→
 * POST 审批/拒绝缝；404/422 就地 danger；成功后 overview 查询失效重取。
 * 拒绝携带拒因（chips + note）单行直通；组级「全批/全拒」由 LeadsBlock
 * 前端逐行 fan-out 到本页两个 mutation。标题行 L1 自治位徽标读 overview
 * leads.autonomy（0=纯人工 / 1=L1 自动批准+预算消费）。
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
      // F1 errText 单家（R3-F3 归一：不再手抄同形表达式）。
      setApproveError(errText(error));
      setBusyLeadId(null);
    },
    onSuccess: () => {
      setBusyLeadId(null);
      // 批准翻转账本 → overview（LeadsBlock 数据源）失效重取。
      void queryClient.invalidateQueries({ queryKey: ["overview", roomId] });
    },
  });
  // D9：拒绝缝宿主——拒因行内已选（全空合法 = 服务端 NULL/NULL 留档）。
  const reject = useMutation({
    mutationFn: ({ leadId, chips, note }: { leadId: string; chips: string[]; note: string }) =>
      api.rejectLead(roomId, leadId, { chips, note }),
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
  // F3：overview 接口收紧后 pending 变体 data 类型含 undefined（F3 单宽限；他单重做消费面时收回）。
  const autonomy = overview.data?.leads?.autonomy === 1;
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
      {rejectError && (
        <span className="badge danger" data-testid="lead-reject-error">
          {rejectError}
        </span>
      )}
      <LeadsBlock
        leads={overview.data?.leads ?? {}}
        onApprove={(leadId) => approve.mutate(leadId)}
        onReject={(leadId, chips, note) => reject.mutate({ leadId, chips, note })}
        busyLeadId={busyLeadId}
        busyRejectId={busyRejectId}
      />
    </section>
  );
}
