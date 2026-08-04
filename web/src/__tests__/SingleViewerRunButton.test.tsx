/**
 * SingleViewerRunButton（viewer 单查触发钮，ViewerTree 空轴引导位）的单飞互斥契约钉（R3-F1）：
 * - 成功：submit 登记进全局 RunTracker（与 Viewers 单查同源）；
 * - 409：错文里的在飞 run_id 提取后转为跟随，入口翻成跟随态（同 KindRunButton 契约），
 *   不就地裸报错。
 * R3#4：submitted 不是永久锁——tracker 见到 tracked run 终态后释放，入口恢复可再发。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { type RunRecordView } from "../api";
import { SingleViewerRunButton } from "../components/SingleViewerRunButton";
import { RunTrackerProvider, useRunTracker } from "../components/RunTracker";

interface FetchPlan {
  [path: string]: Array<{ status: number; body: string }>;
}

function stubFetchPlan(plan: FetchPlan) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      const queue = plan[path];
      const next =
        queue && queue.length > 0
          ? queue.shift()!
          : { status: 404, body: JSON.stringify({ error: `未存根的请求：${path}` }) };
      return {
        ok: next.status >= 200 && next.status < 300,
        status: next.status,
        text: async () => next.body,
      } as Response;
    }),
  );
}

/** run 轮询帧（shape 同 RunTracker.test 的 makeRecord：渲染面只读 status/events 等键）。 */
function makeRecord(over: Partial<RunRecordView>): RunRecordView {
  return {
    run_id: "beef1234aa",
    kind: "viewer",
    viewer_uid: "1001",
    force: false,
    status: "collecting",
    started_at: "2026-08-04T00:00:00+00:00",
    finished_at: null,
    partial: false,
    outcome: null,
    events: [],
    ...over,
  };
}

function harness(node: React.ReactNode) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  function Probe() {
    const tracker = useRunTracker();
    return (
      <div>
        {node}
        <span data-testid="run-id">{tracker.runId ?? "none"}</span>
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
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("SingleViewerRunButton 单查触发钮（kind=viewer 单飞互斥契约，R3-F1）", () => {
  it("提交成功 → run_id 登记进 tracker，入口翻转为「已提交」跟随态", async () => {
    stubFetchPlan({
      "/api/runs": [{ status: 202, body: JSON.stringify({ run_id: "beef1234aa" }) }],
    });
    harness(<SingleViewerRunButton vid="1001" />);
    fireEvent.click(screen.getByRole("button", { name: "触发该观众单查" }));
    await waitFor(() => expect(screen.getByTestId("run-id").textContent).toBe("beef1234aa"));
    await waitFor(() => expect(screen.getByRole("button", { name: /已提交/ })).toBeTruthy());
  });

  it("409（错文含在飞 run_id）→ tracker 转入在飞 id，入口翻转为跟随态、不裸报错", async () => {
    stubFetchPlan({
      "/api/runs": [
        { status: 409, body: JSON.stringify({ error: "已有进行中的 run（a1b2c3d4），与其互斥，待其到达终态后再触发" }) },
      ],
    });
    harness(<SingleViewerRunButton vid="1001" />);
    fireEvent.click(screen.getByRole("button", { name: "触发该观众单查" }));
    // 错文里的在飞 id 转入 RunTracker 跟随。
    await waitFor(() => expect(screen.getByTestId("run-id").textContent).toBe("a1b2c3d4"));
    // 入口状态翻转为跟随态（已提交 → 进度由页头徽标接管），且无错误徽标。
    await waitFor(() => expect(screen.getByRole("button", { name: /已提交/ })).toBeTruthy());
    expect(screen.queryByText(/409/)).toBeNull();
  });

  it("R3#4：在飞中禁发；tracker 见终态后释放 submitted，入口恢复可再发", async () => {
    stubFetchPlan({
      "/api/runs": [{ status: 202, body: JSON.stringify({ run_id: "beef1234aa" }) }],
      // 第一帧在飞（collecting），第二帧终态（done）——RUN_POLL_INTERVAL_MS=1500 一拍翻终。
      "/api/runs/beef1234aa": [
        { status: 200, body: JSON.stringify(makeRecord({ status: "collecting" })) },
        { status: 200, body: JSON.stringify(makeRecord({ status: "done" })) },
      ],
    });
    harness(<SingleViewerRunButton vid="1001" />);
    fireEvent.click(screen.getByRole("button", { name: "触发该观众单查" }));
    // 在飞中：入口翻转「已提交」且禁发。
    const submitted = await waitFor(() => screen.getByRole("button", { name: /已提交/ }));
    expect((submitted as HTMLButtonElement).disabled).toBe(true);
    // 终态拍到达 → submitted 释放 → 入口回「触发该观众单查」且可再发。
    const rearmed = await waitFor(
      () => screen.getByRole("button", { name: "触发该观众单查" }),
      { timeout: 4000 },
    );
    expect((rearmed as HTMLButtonElement).disabled).toBe(false);
  });
});