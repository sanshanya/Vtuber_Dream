/**
 * Z5c 时效位钉团：
 * - ai_stale=true → 行亮「信源已更新·待重判」徽标（badge.action，title 直陈熄灭路径）；
 * - ai_stale=false / null → 不亮（绿灯与无参考旧结论两面无差别安静）。
 * - 行面其余字段透视不受时效位影响。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RunTrackerProvider } from "../components/RunTracker";
import { Viewers } from "../pages/Viewers";

function stubFetch(body: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, text: async () => body }) as Response),
  );
}

function renderViewers() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <RunTrackerProvider>
        <Viewers roomId="983" />
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
