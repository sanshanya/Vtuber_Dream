/**
 * Streamer 首页钉团：
 * - 空态判别（ag5-F6：404 status 特判，不再子串匹配 message）；
 * - 概述 500 且文案含 "collection" → 不许误吞成空态（原裸子串 bug 复现形）；
 * - synthetic 徽标 = collection/ai/situation 任一析取（W3/X2补丁钉）；
 * - Z2：主播卡 profile 透传（名字/粉丝徽标/头像 no-referrer）与 profile 缺档空态；
 * - Z2：executive_summary 走 Markdown（## → h3、- ** → li strong），不再糊整段 <p>。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Streamer } from "../pages/Streamer";

function stubFetch(status: number, body: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: status < 300, status, text: async () => body }) as Response),
  );
}

/** URL 判别 stub：overview 与 viewers 两面分别供给（舰长栏钉面需要真数组）。 */
function stubFetchMap(map: Record<string, { status: number; body: unknown }>) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      for (const [needle, stub] of Object.entries(map)) {
        if (url.includes(needle)) {
          return {
            ok: stub.status < 300,
            status: stub.status,
            text: async () => JSON.stringify(stub.body),
          } as Response;
        }
      }
      return { ok: false, status: 404, text: async () => JSON.stringify({ error: "?" }) } as Response;
    }),
  );
}

function renderStreamer() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <Streamer roomId="983" />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Streamer 空态判别（ag5-F6）", () => {
  it("overview 404 → 空态引导", async () => {
    stubFetch(404, JSON.stringify({ error: "尚无 collection 完成快照" }));
    renderStreamer();
    await waitFor(() => expect(screen.getByText(/还没有采集数据/)).toBeTruthy());
  });

  it("500 且文案恰好含 collection → 原样报错，不得误吞空态（裸子串 bug 复现形）", async () => {
    stubFetch(500, JSON.stringify({ error: "collection 写入失败：磁盘只读" }));
    renderStreamer();
    await waitFor(() => expect(screen.getByText(/磁盘只读/)).toBeTruthy());
    expect(screen.queryByText(/还没有采集数据/)).toBeNull();
  });

  // W3/X2补丁钉：synthetic 徽标判定是 collection/ai/situation 任一分段析取——
  // 单点 ai=true 即亮（写位随工件周期漂移，前端不许绑定单一来源位）。
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
    renderStreamer();
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
    renderStreamer();
    await waitFor(() => expect(screen.getByText("真实态势")).toBeTruthy());
    expect(screen.queryByTestId("situ-synthetic")).toBeNull();
  });
});

describe("Streamer 主播卡（Z2）", () => {
  const base = {
    room_id: "1790370612",
    streamer_uid: "3546595083683995",
    collection: { status: "complete", finished_at: "2026-08-05T00:00:00+00:00" },
    ai: { status: "complete" },
  };

  it("profile 透传：名字 + 粉丝/关注徽标 + 头像 no-referrer 防盗链", async () => {
    stubFetch(
      200,
      JSON.stringify({
        ...base,
        streamer: {
          uid: "3546595083683995",
          name: "演示主播",
          face: "https://i1.hdslb.com/bfs/face/demo.jpg",
          level: 5,
          followers: 1727,
          following: 84,
          sign: "签名一行",
        },
      }),
    );
    renderStreamer();
    await waitFor(() => expect(screen.getByText("演示主播")).toBeTruthy());
    expect(screen.getByText(/粉丝 1,727/)).toBeTruthy();
    expect(screen.getByText(/关注 84/)).toBeTruthy();
    expect(screen.getByText("Lv5")).toBeTruthy();
    const face = document.querySelector<HTMLImageElement>("img.streamer-face");
    expect(face?.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(screen.getByText("签名一行")).toBeTruthy();
  });

  it("profile 缺档 → 引导文案 + 双外链仍在，不臆造资料", async () => {
    stubFetch(200, JSON.stringify({ ...base, streamer: null }));
    renderStreamer();
    await waitFor(() => expect(screen.getByText(/尚无主播资料/)).toBeTruthy());
    expect(screen.getByText(/B站空间/).getAttribute("href")).toBe(
      "https://space.bilibili.com/3546595083683995",
    );
    expect(screen.getByText(/直播间 1790370612/)).toBeTruthy();
    expect(document.querySelector("img.streamer-face")).toBeNull();
  });

  it("executive_summary 走 Markdown：## 出标题、- **粗体** 出列表项 strong", async () => {
    stubFetch(
      200,
      JSON.stringify({
        ...base,
        situation: {
          status: "complete",
          analysis: {
            executive_summary: "## 观众结构\n- **1名舰长**：高价值\n",
            interest_graph: [],
          },
        },
      }),
    );
    renderStreamer();
    await waitFor(() => {
      const heading = document.querySelector(".markdown h3");
      expect(heading?.textContent).toBe("观众结构");
      const strong = document.querySelector(".markdown li strong");
      expect(strong?.textContent).toBe("1名舰长");
    });
  });

  it("Z2b：舰长栏=采集一发的名单见人——chip 渲染名字+AI状态+态势深链", async () => {
    stubFetchMap({
      overview: { status: 200, body: { ...base } },
      viewers: {
        status: 200,
        body: [
          { uid: "313200344", name: "丸丸丸丸丸丸子_F版", ai_status: "complete", ai_completed: true },
          { uid: "1712756077", name: "Skystairs", ai_status: "complete", ai_completed: true },
        ],
      },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("guard-strip")).toBeTruthy());
    const chips = document.querySelectorAll(".guard-chip");
    expect(chips.length).toBe(2);
    expect(chips[0].textContent).toContain("丸丸丸丸丸丸子_F版");
    expect(chips[0].getAttribute("href")).toBe("#/viewers/313200344/tree");
    expect(screen.getByText("全部舰长 →").getAttribute("href")).toBe("#/viewers");
  });

  it("Z2b：舰长名单空 → 引导空态，不悬挂 loading", async () => {
    stubFetchMap({
      overview: { status: 200, body: { ...base } },
      viewers: { status: 200, body: [] },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByText(/舰长名单为空/)).toBeTruthy());
  });
});
