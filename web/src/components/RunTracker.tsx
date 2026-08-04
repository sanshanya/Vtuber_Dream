/**
 * RunTracker（ag4-F1/ag5-F3 裁定）：run 追踪从 RunButton 组件局部态提升为
 * App 级共享 context——任何提交口（hero 触发钮、Viewers 单查、ViewerTree 空轴）
 * 都把 run_id 登记进同一位置；轮询、终态 invalidate、丢失提示三链在此单点兑现：
 *
 * - 轮询只在 active 期启用（D8 口径不变）；
 * - 终态当拍失效全部数据查询，但排除 ["run"] 键族自身（ag4-F4 收窄）；
 * - 轮询 404（服务重启丢内存 registry）→ 显式「run 记录已丢失」提示（ag4-F3/ag5-F1），
 *   弹新 track 或 dismissLost 前常驻，不再静默吞掉。
 *
 * design §10「全部页头触发钮」裁决：hero 单点挂载 RunButton（App.tsx），
 * RunButton 只是本 context 的一个渲染面。
 */
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { api, isApiError, type RunRecordView } from "../api";
import { RUN_POLL_INTERVAL_MS } from "../constants";

/** design §10 状态机字面的终态集合（server registry::RUN_STATES 同源）。 */
export const TERMINAL_STATES = ["done", "failed"];

export function isRunActive(record: RunRecordView | undefined): boolean {
  return record !== undefined && !TERMINAL_STATES.includes(record.status);
}

export interface RunTrackerValue {
  /** 当前追踪的 run（无追踪 → null）。 */
  runId: string | null;
  /** 最近一次轮询快照；丢失后保留最后一帧供徽标/事件面展示。 */
  record: RunRecordView | undefined;
  active: boolean;
  /** 丢失提示（404 重启丢 registry 等）；track 新 run 自动清。 */
  lost: string | null;
  track: (runId: string) => void;
  dismissLost: () => void;
}

const RunTrackerContext = createContext<RunTrackerValue | null>(null);

export function useRunTracker(): RunTrackerValue {
  const context = useContext(RunTrackerContext);
  if (!context) {
    throw new Error("useRunTracker 必须挂在 <RunTrackerProvider> 内");
  }
  return context;
}

export function RunTrackerProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [runId, setRunId] = useState<string | null>(null);
  const [lost, setLost] = useState<string | null>(null);
  const [lastRecord, setLastRecord] = useState<RunRecordView | undefined>(undefined);

  const record = useQuery({
    queryKey: ["run", runId],
    queryFn: () => api.run(runId!),
    enabled: runId !== null,
    refetchInterval: (query) => (isRunActive(query.state.data) ? RUN_POLL_INTERVAL_MS : false),
  });

  // 滞留最后一帧：runId 被清（丢失/用户 dismiss）后徽标仍能显示终局状态。
  const data = record.data ?? lastRecord;
  useEffect(() => {
    if (record.data !== undefined) setLastRecord(record.data);
  }, [record.data]);

  // ag4-F3/ag5-F1：轮询错误显形——404 语义 = 服务重启丢 registry。
  useEffect(() => {
    if (runId !== null && record.isError) {
      const error: unknown = record.error;
      setLost(
        isApiError(error) && error.status === 404
          ? "run 记录已丢失（服务重启？）"
          : `run 轮询失败：${error instanceof Error ? error.message : String(error)}`,
      );
      setRunId(null);
    }
  }, [runId, record.isError, record.error]);

  // ag4-F1：终态当拍失效数据查询（ag4-F4：排除 ["run"] 键族自身）。
  const status = record.data?.status;
  const previousStatus = useRef<string | undefined>(undefined);
  useEffect(() => {
    const before = previousStatus.current;
    previousStatus.current = status;
    if (status !== undefined && status !== before && TERMINAL_STATES.includes(status)) {
      void queryClient.invalidateQueries({
        predicate: (query) => query.queryKey[0] !== "run",
      });
    }
  }, [status, queryClient]);

  const track = useCallback((next: string) => {
    previousStatus.current = undefined;
    setLost(null);
    setRunId(next);
  }, []);
  const dismissLost = useCallback(() => {
    setLost(null);
    setLastRecord(undefined);
  }, []);

  const value = useMemo<RunTrackerValue>(
    () => ({ runId, record: data, active: isRunActive(data), lost, track, dismissLost }),
    [runId, data, lost, track, dismissLost],
  );
  return <RunTrackerContext.Provider value={value}>{children}</RunTrackerContext.Provider>;
}
