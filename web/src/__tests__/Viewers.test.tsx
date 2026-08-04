/**
 * Z5c 时效位钉团：
 * - ai_stale=true → 行亮「信源已更新·待重判」徽标（badge.action，title 直陈熄灭路径）；
 * - ai_stale=false / null → 不亮（绿灯与无参考旧结论两面无差别安静）。
 * - 行面其余字段透视不受时效位影响。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RunTrackerProvider, useRunTracker } from "../components/RunTracker";
import { Viewers } from "../pages/Viewers";

function stubFetch(body: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, text: async () => body }) as Response),
  );
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

function renderViewers() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  // Probe 把全局 RunTracker 的 runId 摆到页面上——跟随断言读它。
  function Probe() {
    const tracker = useRunTracker();
    return (
      <div>
        <Viewers roomId="983" />
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

function viewerRow(over: Record<string, unknown>) {
  return {
    uid: "1001",
    name: "观众甲",
    face: null,
    guard_level: 3,
    medal_level: 12,
    collected_at: "2026-08-03T07:52:20+00:00",
    ai_status: "complete",
    ai_completed: true,
    ai_stale: null,
    ...over,
  };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Viewers 时效位徽标（Z5c）", () => {
  it("ai_stale=true → 亮「信源已更新·待重判」，且状态徽标本体保留", async () => {
    stubFetch(JSON.stringify([viewerRow({ uid: "1001", ai_stale: true })]));
    renderViewers();
    await waitFor(() => expect(screen.getByTestId("ai-stale-badge")).toBeTruthy());
    expect(screen.getByText("complete")).toBeTruthy();
    const badge = screen.getByTestId("ai-stale-badge");
    expect(badge.textContent).toBe("信源已更新·待重判");
    expect(badge.getAttribute("title")).toMatch(/重跑「舰长 AI 分析」后熄灭/);
  });

  it("ai_stale=false → 不亮（时效位绿灯安静）", async () => {
    stubFetch(JSON.stringify([viewerRow({ uid: "1001", ai_stale: false })]));
    renderViewers();
    await waitFor(() => expect(screen.getByText("complete")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge")).toBeNull();
  });

  it("ai_stale=null → 不亮（无参考旧结论的安静面）", async () => {
    stubFetch(JSON.stringify([viewerRow({ uid: "1001", ai_stale: null, ai_status: null, ai_completed: false })]));
    renderViewers();
    await waitFor(() => expect(screen.getByText("未运行")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge")).toBeNull();
  });
});

describe("Viewers 单查入口的单飞互斥契约（R3-F1）：409 → 转入跟随，不裸报错", () => {
  it("409（错文含在飞 run_id）→ tracker 收到在飞 id，页面现跟随徽标，无拒单错文", async () => {
    stubFetchPlan({
      "/api/rooms/983/viewers": [{ status: 200, body: "[]" }],
      "/api/runs": [
        { status: 409, body: JSON.stringify({ error: "已有进行中的 run（a1b2c3d4），与其互斥，待其到达终态后再触发" }) },
      ],
    });
    renderViewers();
    // 空池布景（D3 冷启动引导位）——只有名单为空才出现单查入口。
    await waitFor(() => expect(screen.getByTestId("empty-pool-hint")).toBeTruthy());
    fireEvent.change(screen.getByLabelText("单查观众 uid"), { target: { value: "1003" } });
    fireEvent.click(screen.getByRole("button", { name: "单查该观众" }));
    // 单查 POST 冲突：错文里在飞 id 转入 RunTracker 跟随。
    await waitFor(() => expect(screen.getByTestId("run-id").textContent).toBe("a1b2c3d4"));
    // 页面上出现跟随态徽标，且不出现「提交被拒」裸错。
    await waitFor(() => expect(screen.getByText(/转为跟随其进度/)).toBeTruthy());
    expect(screen.queryByText(/单查提交被拒/)).toBeNull();
  });
});
