/**
 * Dashboard 空态判别钉（ag5-F6：404 status 特判，不再子串匹配 message）：
 * - overview 404 → 引导文案（含「还没有采集数据」）；
 * - overview 500 且文案含 "collection" 字样 → 不许误吞成空态（原裸子串 bug 复现形）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Dashboard } from "../pages/Dashboard";

function stubFetch(status: number, body: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: status < 300, status, text: async () => body }) as Response),
  );
}

function renderDashboard() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <Dashboard roomId="983" />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Dashboard 空态判别（ag5-F6）", () => {
  it("overview 404 → 空态引导", async () => {
    stubFetch(404, JSON.stringify({ error: "尚无 collection 完成快照" }));
    renderDashboard();
    await waitFor(() => expect(screen.getByText(/还没有采集数据/)).toBeTruthy());
  });

  it("500 且文案恰好含 collection → 原样报错，不得误吞空态（裸子串 bug 复现形）", async () => {
    stubFetch(500, JSON.stringify({ error: "collection 写入失败：磁盘只读" }));
    renderDashboard();
    await waitFor(() => expect(screen.getByText(/磁盘只读/)).toBeTruthy());
    expect(screen.queryByText(/还没有采集数据/)).toBeNull();
  });

  // W3/X2补丁钉：synthetic 徽标判定是 collection/ai/situation 任一分段析取——
  // 单点 id=true 即亮（写位随工件周期漂移，前端不许绑定单一来源位）。
  it("synthetic 任一分段析取：仅 ai.synthetic_demo=true 也出徽标", async () => {
    stubFetch(
      200,
      JSON.stringify({
        collection: { status: "complete", finished_at: "2026-08-05T00:00:00+00:00" },
        ai: { status: "complete", synthetic_demo: true },
        situation: {
          status: "complete",
          analysis: { executive_summary: "合成态势", interest_graph: [] },
        },
      }),
    );
    renderDashboard();
    await waitFor(() => expect(screen.getByTestId("situ-synthetic")).toBeTruthy());
  });

  it("synthetic 三分段全缺席 → 不臆造徽标", async () => {
    stubFetch(
      200,
      JSON.stringify({
        collection: { status: "complete", finished_at: "2026-08-05T00:00:00+00:00" },
        ai: { status: "complete" },
        situation: {
          status: "complete",
          analysis: { executive_summary: "真实态势", interest_graph: [] },
        },
      }),
    );
    renderDashboard();
    await waitFor(() => expect(screen.getByText("真实态势")).toBeTruthy());
    expect(screen.queryByTestId("situ-synthetic")).toBeNull();
  });
});
