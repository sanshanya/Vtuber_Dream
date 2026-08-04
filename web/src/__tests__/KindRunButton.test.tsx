/**
 * Z4 动作平面钉团：
 * - 全量感知钮：一击不飞（先展开确认段），二击「确认触发」才 POST kind=full。
 * - KindRunButton：一击提交对应 kind；409 错文提取在飞 run_id 转为跟随；422 就地报错。
 * - hero 去钮后：RunStatusBadge 独立挂载负责状态回报。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { KindRunButton } from "../components/KindRunButton";
import { RunButton } from "../components/RunButton";
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

describe("RunButton 谨慎双段确认（Z4 全量感知敏感钮）", () => {
  it("第一击只展开确认段，不发起请求", () => {
    stubFetchPlan({});
    harness(<RunButton viewerCount={5} />);
    fireEvent.click(screen.getByRole("button", { name: "触发全量感知" }));
    expect(screen.getByTestId("full-run-confirm")).toBeTruthy();
    // 成本/时长与互斥语义必须在确认段直陈。
    expect(screen.getByText(/DeepSeek 价目上界/)).toBeTruthy();
    expect(screen.getByText(/409 互斥/)).toBeTruthy();
    expect(screen.getByText(/当前名单 5 人/)).toBeTruthy();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("取消 → 回到未确认态，不提交", () => {
    stubFetchPlan({});
    harness(<RunButton />);
    fireEvent.click(screen.getByRole("button", { name: "触发全量感知" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByTestId("full-run-confirm")).toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("第二击「确认触发」→ POST kind=full 并登记追踪", async () => {
    stubFetchPlan({ "/api/runs": [{ status: 202, body: JSON.stringify({ run_id: "abc12345" }) }] });
    harness(<RunButton />);
    fireEvent.click(screen.getByRole("button", { name: "触发全量感知" }));
    fireEvent.click(screen.getByRole("button", { name: "确认触发全量感知" }));
    await waitFor(() => expect(screen.getByTestId("run-id").textContent).toBe("abc12345"));
  });
});

describe("KindRunButton 分层动作钮（Z4）", () => {
  it("一击提交对应 kind 并在组件内反馈已提交", async () => {
    stubFetchPlan({ "/api/runs": [{ status: 202, body: JSON.stringify({ run_id: "beef9900aa" }) }] });
    harness(<KindRunButton kind="collect_guards" note="驳回文案占位" />);
    fireEvent.click(screen.getByRole("button", { name: "舰长采集" }));
    await waitFor(() => expect(screen.getByTestId("run-id").textContent).toBe("beef9900aa"));
    await waitFor(() => expect(screen.getByText(/已提交 beef9900/)).toBeTruthy());
  });

  it("409（错文含在飞 run_id）→ 转为跟随在飞 run，不就地报错", async () => {
    stubFetchPlan({
      "/api/runs": [
        { status: 409, body: JSON.stringify({ error: "已有进行中的 run（a1b2c3d4），待其到达终态后再触发" }) },
      ],
    });
    harness(<KindRunButton kind="ai_audience" />);
    fireEvent.click(screen.getByRole("button", { name: "主播 AI 分析" }));
    await waitFor(() => expect(screen.getByTestId("run-id").textContent).toBe("a1b2c3d4"));
    await waitFor(() => expect(screen.getByText(/跟随其进度/)).toBeTruthy());
  });

  it("422 参数面 → 就地 danger 报错，不登记追踪", async () => {
    stubFetchPlan({
      "/api/runs": [{ status: 422, body: JSON.stringify({ error: "kind=ai_viewers 不接受 force" }) }],
    });
    harness(<KindRunButton kind="ai_viewers" />);
    fireEvent.click(screen.getByRole("button", { name: "舰长 AI 分析" }));
    await waitFor(() => expect(screen.getByText(/不接受 force/)).toBeTruthy());
    expect(screen.getByTestId("run-id").textContent).toBe("none");
  });
});
