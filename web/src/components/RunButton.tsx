import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { api, type RunRecordView } from "../api";
import { RUN_EVENTS_CAP, RUN_POLL_INTERVAL_MS } from "../constants";
import { fmtTime } from "../format";

const TERMINAL_STATES = ["done", "failed"];

export function isRunActive(record: RunRecordView | undefined): boolean {
  return record !== undefined && !TERMINAL_STATES.includes(record.status);
}

/**
 * 页头触发钮（design §10：全部页头）+ 轮询（D8：仅 run active 期启用）。
 * run 到达终态 → 当拍失效全部数据查询（面板/viewers/tree/graph）。
 */
export function RunButton() {
  const queryClient = useQueryClient();
  const [runId, setRunId] = useState<string | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const record = useQuery({
    queryKey: ["run", runId],
    queryFn: () => api.run(runId!),
    enabled: runId !== null,
    refetchInterval: (query) => (isRunActive(query.state.data) ? RUN_POLL_INTERVAL_MS : false),
  });
  const active = isRunActive(record.data);
  const status = record.data?.status;
  const previousStatus = useRef<string | undefined>(undefined);
  useEffect(() => {
    const before = previousStatus.current;
    previousStatus.current = status;
    if (status !== undefined && status !== before && TERMINAL_STATES.includes(status)) {
      queryClient.invalidateQueries();
    }
  }, [status, queryClient]);

  async function trigger() {
    setSubmitError(null);
    try {
      const { run_id } = await api.startRun({ kind: "full" });
      setRunId(run_id);
    } catch (error) {
      setSubmitError(String(error instanceof Error ? error.message : error));
    }
  }

  const data = record.data;
  const events = (data?.events ?? []).slice(-RUN_EVENTS_CAP);
  return (
    <span className="run-trigger">
      <button className="primary" disabled={active} onClick={() => void trigger()}>
        {active ? "运行中…" : "触发全量感知"}
      </button>
      {data && (
        <span className={`badge run-status-${data.status}`} title={fmtTime(data.started_at)}>
          {data.status}
          {data.partial ? "(partial)" : ""}
        </span>
      )}
      {submitError && <span className="badge danger">{submitError}</span>}
      {active && events.length > 0 && (
        <details className="run-events">
          <summary>events ({events.length})</summary>
          <pre>{events.join("\n")}</pre>
        </details>
      )}
    </span>
  );
}
