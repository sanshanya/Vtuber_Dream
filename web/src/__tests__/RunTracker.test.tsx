/**
 * RunTracker 共享层钉（ag4-F1/ag4-F3/ag4-F4/ag5-F1/F2/F3 裁定兑现面）：
 * - track 任何来源（单查/全量）→ 同一全局位可见轮询记录；
 * - 终态当拍 invalidateQueries（且排除 ["run"] 键族）；
 * - 轮询 404 → 「run 记录已丢失（服务重启？）」显式提示，不再静默；
 * - 终态（failed）events 仍可读（RunButton 不再按 active 门显）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { api, type RunRecordView } from "../api";
import { RunButton, partialTitle } from "../components/RunButton";
import { RunTrackerProvider, useRunTracker } from "../components/RunTracker";

function makeRecord(over: Partial<RunRecordView>): RunRecordView {
  return {
    run_id: "r-1",
    kind: "viewer",
    viewer_uid: "1003",
    force: false,
    status: "collecting",
    started_at: "2026-08-05T00:00:00+00:00",
    finished_at: null,
    partial: false,
    outcome: null,
    events: [],
    ...over,
  };
}

interface FetchPlan {
  [path: string]: Array<{ status: number; body: string }>;
}

function stubFetchPlan(plan: FetchPlan) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      const queue = plan[path];
      const next = queue && queue.length > 0 ? queue.shift()! : { status: 404, body: JSON.stringify({ error: `未存根的请求：${path}` }) };
      return {
        ok: next.status >= 200 && next.status < 300,
        status: next.status,
        text: async () => next.body,
      } as Response;
    }),
  );
}

function harness() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  function Probe() {
    const tracker = useRunTracker();
    return (
      <div>
        <button onClick={() => tracker.track("r-1")}>track</button>
        <span data-testid="run-id">{tracker.runId ?? "none"}</span>
        <RunButton />
      </div>
    );
  }
  render(
    <QueryClientProvider client={queryClient}>
      <RunTrackerProvider>
        <Probe />
      </RunTrackerProvider>
    </QueryClientProvider>,
  );
  return { queryClient };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("RunTracker 共享追踪", () => {
  it("track 进场（任意来源）→ 全局位可见 runId 且轮询呈现状态徽标", async () => {
    stubFetchPlan({
      "/api/runs/r-1": [{ status: 200, body: JSON.stringify(makeRecord({ status: "collecting" })) }],
    });
    harness();
    fireEvent.click(screen.getByText("track"));
    expect(screen.getByTestId("run-id").textContent).toBe("r-1");
    await waitFor(() => expect(screen.getByText("collecting")).toBeTruthy());
  });

  it("终态当拍触发 invalidateQueries（排除 run 键族自身的谓词形态）", async () => {
    const { queryClient } = (() => {
      stubFetchPlan({
        "/api/runs/r-1": [{ status: 200, body: JSON.stringify(makeRecord({ status: "done" })) }],
      });
      return harness();
    })();
    const spy = vi.spyOn(queryClient, "invalidateQueries");
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByText("done")).toBeTruthy());
    await waitFor(() => expect(spy).toHaveBeenCalled());
    // 谓词实参抽取走口袋形状断言（InvalidateQueryFilters 的 readonly 阵列不碍运行时抽查）。
    const predicates = spy.mock.calls
      .map(
        (call) =>
          (
            call[0] as
              | { predicate?: (query: { queryKey: readonly unknown[] }) => boolean }
              | undefined
          )?.predicate,
      )
      .filter((p): p is NonNullable<typeof p> => typeof p === "function");
    const predicate = predicates[predicates.length - 1];
    expect(predicate).toBeDefined();
    if (!predicate) throw new Error("invalidateQueries 未收到谓词");
    expect(predicate({ queryKey: ["run", "r-1"] })).toBe(false);
    expect(predicate({ queryKey: ["viewers", "983"] })).toBe(true);
    spy.mockRestore();
  });

  it("轮询 404 → 显式「run 记录已丢失（服务重启？）」，不静默吞（ag5-F1）", async () => {
    stubFetchPlan({
      "/api/runs/r-1": [
        { status: 200, body: JSON.stringify(makeRecord({ status: "collecting" })) },
        { status: 404, body: JSON.stringify({ error: "run r-1 不存在" }) },
      ],
    });
    harness();
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByText("collecting")).toBeTruthy());
    await waitFor(() => expect(screen.getByText(/run 记录已丢失/)).toBeTruthy(), {
      timeout: 4000,
    });
    expect(screen.getByTestId("run-id").textContent).toBe("none");
  });

  it("ag5-F2：failed 终态仍渲染 events 与 outcome.error（不再按 active 门显）", async () => {
    stubFetchPlan({
      "/api/runs/r-1": [
        {
          status: 200,
          body: JSON.stringify(
            makeRecord({
              status: "failed",
              outcome: { error: "采集 404" },
              events: ["[runs] 触发 kind=viewer", "[runs] 状态 → failed"],
            }),
          ),
        },
      ],
    });
    harness();
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByText("采集 404")).toBeTruthy());
    expect(screen.getByText(/events \(2\)/)).toBeTruthy();
  });

  it("partialTitle：partial 徽标 explanation 含「观众级失败」语义", () => {
    const record = makeRecord({ status: "done", partial: true });
    expect(partialTitle(record)).toContain("观众级失败");
    expect(partialTitle({ ...record, partial: false })).not.toContain("观众级失败");
  });
});
