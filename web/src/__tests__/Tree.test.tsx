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

describe("ViewerTree Episode 时间戳语义徽标（R2 批6 D7）", () => {
  it("双行混排：published 行落「发布于」、缺 published 行落「采集于」，tooltip 文案各自钉", async () => {
    renderTree({
      episodes: [
        {
          episode_id: "e-pub",
          source: "bangumi",
          event_type: "ep",
          published_at: "2026-08-03T03:06:16+00:00",
          observed_at: "2026-08-03T10:00:00+00:00",
          title: "平台给了行为时刻的行",
        },
        {
          episode_id: "e-only-obs",
          source: "bangumi",
          event_type: "ep",
          observed_at: "2026-08-03T03:06:16+00:00",
          title: "平台没给只留采集时刻的行",
        },
      ],
      mentions: [],
    });
    await waitFor(() => expect(screen.getByTestId("episode-ts-e-pub")).toBeTruthy());
    const published = screen.getByTestId("episode-ts-e-pub");
    const collected = screen.getByTestId("episode-ts-e-only-obs");
    // 徽标各自落位（恒落其一：两徽标不互串）。
    expect(published.textContent).toContain("发布于");
    expect(published.textContent).not.toContain("采集于");
    expect(collected.textContent).toContain("采集于");
    expect(collected.textContent).not.toContain("发布于");
    // tooltip 文案钉（D7 验收原文）。
    expect(published.getAttribute("title")).toBe("发布于=平台显示的行为时刻");
    expect(collected.getAttribute("title")).toBe("采集于=我们看到这条的时刻（非行为时刻）");
  });

  it("published_at 为空串 → 视为平台没给，回落「采集于」", async () => {
    renderTree({
      episodes: [
        {
          episode_id: "e-empty",
          source: "bangumi",
          event_type: "ep",
          published_at: "",
          observed_at: "2026-08-03T03:06:16+00:00",
        },
      ],
      mentions: [],
    });
    await waitFor(() => expect(screen.getByTestId("episode-ts-e-empty")).toBeTruthy());
    const badge = screen.getByTestId("episode-ts-e-empty");
    expect(badge.textContent).toContain("采集于");
    expect(badge.getAttribute("title")).toBe("采集于=我们看到这条的时刻（非行为时刻）");
  });
});