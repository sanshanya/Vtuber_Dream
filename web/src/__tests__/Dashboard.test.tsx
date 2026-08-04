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
});
