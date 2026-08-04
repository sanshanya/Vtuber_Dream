/**
 * 个人树面（/tree）的 Z5c 时效位钉团——与 Viewers.test.tsx 同形三钉：
 * - ai_stale=true → 感知/AI 区块亮「信源已更新·待重判」徽标（ai-stale-badge-tree）；
 * - ai_stale=false / null → 不亮（绿灯与无参考旧结论两面无差别安静）。
 * 断言一律 getByTestId 锚点（Z5 教训：文案与 note 可能同语撞车，不用文本全文匹配）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RunTrackerProvider } from "../components/RunTracker";
import { ViewerTree } from "../pages/ViewerTree";

function stubFetch(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, text: async () => JSON.stringify(body) }) as Response),
  );
}

const BASE_TREE = {
  uid: "demo-1",
  viewer: {
    schema_version: 1,
    viewer: { name: "演示观众A" },
    profile: { face: "" },
    collected_at: "2026-08-03T07:52:20+00:00",
  },
  ai: { status: "complete" },
  ai_stale: null,
  episodes: [],
  mentions: [],
};

function renderTree(over: Record<string, unknown>) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  stubFetch({ ...BASE_TREE, ...over });
  render(
    <QueryClientProvider client={queryClient}>
<RunTrackerProvider>
        <ViewerTree roomId="983" vid="demo-1" />
      </RunTrackerProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("ViewerTree 时效位徽标（Z5c）", () => {
  it("ai_stale=true → 感知区块亮「信源已更新·待重判」，且 Perception 徽标本体保留", async () => {
    renderTree({ ai_stale: true });
    await waitFor(() => expect(screen.getByTestId("ai-stale-badge-tree")).toBeTruthy());
    const badge = screen.getByTestId("ai-stale-badge-tree");
    expect(badge.textContent).toBe("信源已更新·待重判");
    expect(badge.getAttribute("title")).toMatch(/重跑「舰长 AI 分析」后熄灭/);
    expect(screen.getByText("Perception complete")).toBeTruthy();
  });

  it("ai_stale=false → 不亮（时效位绿灯安静）", async () => {
    renderTree({ ai_stale: false });
    await waitFor(() => expect(screen.getByText("Perception complete")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge-tree")).toBeNull();
  });

  it("ai_stale=null → 不亮（无参考旧结论的安静面）", async () => {
    renderTree({ ai_stale: null, ai: { status: null } });
    await waitFor(() => expect(screen.getByText("Perception 未运行")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge-tree")).toBeNull();
  });
});