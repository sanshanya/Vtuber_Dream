/**
 * Streamer 首页钉团：
 * - 空态判别（404 status 特判，不再子串匹配 message）；
 * - 概述 500 且文案含 "collection" → 不许误吞成空态（原裸子串 bug 复现形）；
 * - 主播卡 profile 透传（名字/粉丝徽标/头像 no-referrer）与 profile 缺档空态；
 * - v2：首卡恒为复盘卡（recap 就绪/null 两态 DOM 序）+ 宏观段退役面
 *   （态势项胶囊/宏观折叠组/executive_summary 直呈/situ-synthetic 徽标随退役段
 *   同葬；页脚 synthetic 徽标亦随刀2 runtime-demo 删除同葬）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RunTrackerProvider } from "../components/RunTracker";
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
      {/* 页面内嵌动作钮（RunButton/KindRunButton）依赖 RunTracker 上下文。 */}
      <RunTrackerProvider>
        <Streamer roomId="983" />
      </RunTrackerProvider>
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

  it("Z3：指标条=旧版站签名（舰长/Episode/Mention/实体/关系；R2 批5 D6：态势项胶囊退役）", async () => {
    stubFetchMap({
      overview: {
        status: 200,
        body: {
          ...base,
          graph_stats: {
            episodes: 998,
            mentions: 652,
            entities: 1473,
            relations: 4477,
            interest_states: 58,
          },
          situation: {
            status: "complete",
            analysis: { executive_summary: "s", situations: [{}, {}, {}] },
          },
        },
      },
      viewers: { status: 200, body: [] },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("kpi-strip")).toBeTruthy());
    const strip = screen.getByTestId("kpi-strip");
    expect(strip.textContent).toContain("998");
    expect(strip.textContent).toContain("1,473");
    expect(strip.textContent).toContain("4,477");
    // 退役实证：胶囊面只剩 5 枚，且「态势项」在指标条零残留。
    expect(strip.textContent).not.toContain("态势项");
    expect(strip.querySelectorAll(".card.stat").length).toBe(5);
  });

  it("v2 P1-1（裁决三）：首卡恒为复盘卡——recap 就绪与 null 两态 DOM 序均领先简报卡", async () => {
    const ready = {
      status: "ready",
      generated_at: "2026-08-05T22:00:00+00:00",
      session: { start: "2026-08-05T21:00:00+00:00", end: "2026-08-05T21:30:00+00:00", rid: "S2" },
      headline: "今晚 3 人来过，1 人回来过",
      speakers: 3,
      returning: { count: 1, base: 3, sessions_back: 1 },
      peak: null,
      repeated: null,
      naming: null,
      unknown: [],
      empty_copy: null,
    };
    for (const recap of [ready, null]) {
      stubFetchMap({
        overview: {
          status: 200,
          body: {
            ...base,
            recap,
            situation: {
              status: "complete",
              analysis: { front_brief: { sentences: [] } },
            },
          },
        },
        viewers: { status: 200, body: [] },
      });
      renderStreamer();
      await waitFor(() => expect(screen.getByTestId("recap-card")).toBeTruthy());
      const recapCard = screen.getByTestId("recap-card");
      const briefingCard = screen.getByTestId("briefing-card");
      expect(
        recapCard.compareDocumentPosition(briefingCard) & Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
      // 退役面零残留（宏观/态势双区都不复存在）。
      expect(screen.queryByTestId("macro-details")).toBeNull();
      // 「整体态势」heading 是退役段的签名（动作区 note 里的同名短语是存活文案）。
      expect(screen.queryByRole("heading", { name: "整体态势" })).toBeNull();
      expect(screen.queryByText(/点开看/)).toBeNull();
      cleanup();
    }
  });

  it("Z3：graph_stats 缺图态 → 指标条落「—」不臆造数字", async () => {
    stubFetchMap({
      overview: { status: 200, body: { ...base, graph_stats: null } },
      viewers: { status: 200, body: [] },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("kpi-strip")).toBeTruthy());
    expect(screen.getByTestId("kpi-strip").textContent).toContain("—");
    expect(screen.getByTestId("kpi-strip").textContent).not.toContain("998");
  });

  it("Z3d：舰长 chip 有 face → 头像 no-referrer；face 空 → 首字回退块", async () => {
    stubFetchMap({
      overview: { status: 200, body: { ...base } },
      viewers: {
        status: 200,
        body: [
          {
            uid: "u1",
            name: "有头",
            face: "https://i0.hdslb.com/bfs/face/a.jpg",
            guard_level: 3,
            medal_level: 25,
            ai_status: "complete",
            ai_completed: true,
          },
          { uid: "u2", name: "无头", face: "", ai_status: "pending", ai_completed: false },
        ],
      },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("guard-strip")).toBeTruthy());
    const img = document.querySelector<HTMLImageElement>(".guard-chip img.avatar-xs");
    expect(img?.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(document.querySelector(".guard-chip .avatar-fallback")?.textContent).toBe("无");
  });
});

describe("Streamer 舰长 strip 时效位徽标（Z5c）", () => {
  const base = {
    room_id: "1790370612",
    streamer_uid: "3546595083683995",
    collection: { status: "complete", finished_at: "2026-08-05T00:00:00+00:00" },
    ai: { status: "complete" },
  };
  const chip = (uid: string, name: string, over: Record<string, unknown>) => ({
    uid,
    name,
    ai_status: "complete",
    ai_completed: true,
    ai_stale: null,
    ...over,
  });

  it("ai_stale=true → chip 亮「信源已更新·待重判」，且落在对号舰长卡", async () => {
    stubFetchMap({
      overview: { status: 200, body: { ...base } },
      viewers: {
        status: 200,
        body: [
          chip("u1", "过期甲", { ai_stale: true }),
          chip("u2", "绿灯乙", { ai_stale: false }),
        ],
      },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("ai-stale-badge-strip")).toBeTruthy());
    const badges = screen.queryAllByTestId("ai-stale-badge-strip");
    expect(badges.length).toBe(1);
    const badge = badges[0];
    expect(badge.textContent).toBe("信源已更新·待重判");
    expect(badge.getAttribute("title")).toMatch(/重跑「舰长 AI 分析」后熄灭/);
    expect(badge.closest(".guard-chip")?.textContent).toContain("过期甲");
    expect(badge.closest(".guard-chip")?.textContent).not.toContain("绿灯乙");
  });

  it("ai_stale=false → 不亮（时效位绿灯安静）", async () => {
    stubFetchMap({
      overview: { status: 200, body: { ...base } },
      viewers: { status: 200, body: [chip("u1", "绿灯乙", { ai_stale: false })] },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("guard-strip")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge-strip")).toBeNull();
  });

  it("ai_stale=null → 不亮（无参考旧结论的安静面）", async () => {
    stubFetchMap({
      overview: { status: 200, body: { ...base } },
      viewers: {
        status: 200,
        body: [chip("u1", "无旧丙", { ai_stale: null, ai_status: "pending", ai_completed: false })],
      },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByText("无旧丙")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge-strip")).toBeNull();
  });
});

describe("Streamer 感知动作收敛与主钮旁预估（R2 批6 D8）", () => {
  const base = {
    room_id: "1790370612",
    streamer_uid: "3546595083683995",
    collection: { status: "complete", finished_at: "2026-08-05T00:00:00+00:00", viewer_count: 22 },
    ai: { status: "complete" },
  };
  const estimateBody = {
    roster_viewers: 22,
    fresh_viewers: 17,
    estimated_cny: 27.0,
    etd_minutes: [13, 27],
  };

  it("首页主钮唯一性：room-actions 内 primary 按钮计数==1，且是「触发全量感知」", async () => {
    stubFetchMap({
      overview: { status: 200, body: base },
      viewers: { status: 200, body: [] },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("run-estimate")).toBeTruthy());
    const actions = screen.getByTestId("room-actions");
    const primaries = actions.querySelectorAll("button.primary");
    expect(primaries.length).toBe(1);
    expect(primaries[0]?.textContent).toContain("触发全量感知");
  });

  it("分层跑次级菜单：收纳 ai_audience 触发钮 + 三个页面引导项", async () => {
    stubFetchMap({
      overview: { status: 200, body: base },
      viewers: { status: 200, body: [] },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("tiered-runs")).toBeTruthy());
    const tiered = screen.getByTestId("tiered-runs");
    // BriefingCard 内还有同标签的 ai_audience 触发钮——菜单内的必须在本容器断言。
    expect(within(tiered).getByRole("button", { name: "主播 AI 分析" })).toBeTruthy();
    expect(tiered.querySelectorAll(".tiered-guide").length).toBe(3);
    const guideHrefs = Array.from(tiered.querySelectorAll<HTMLAnchorElement>(".tiered-guide a")).map(
      (a) => a.getAttribute("href"),
    );
    expect(guideHrefs).toEqual(["#/live", "#/viewers", "#/viewers"]);
  });

  it("主钮旁预估行数字钉：estimated_cny/etd 在场 → 「预估 ≈¥27.00（新鲜 17/22 人）· 约 13~27 分钟」", async () => {
    stubFetchMap({
      overview: { status: 200, body: base },
      viewers: { status: 200, body: [] },
      budget: {
        status: 200,
        body: {
          budget_cny: null,
          estimate: estimateBody,
        },
      },
    });
    renderStreamer();
    await waitFor(() =>
      expect(screen.getByTestId("run-estimate").textContent).toBe(
        "预估 ≈¥27.00（新鲜 17/22 人）· 约 13~27 分钟",
      ),
    );
  });

  it("主钮旁预估行空态：服务端估计全 null → 「预估 —」不臆造", async () => {
    stubFetchMap({
      overview: { status: 200, body: base },
      viewers: { status: 200, body: [] },
      budget: {
        status: 200,
        body: {
          budget_cny: null,
          estimate: {
            roster_viewers: null,
            fresh_viewers: null,
            estimated_cny: null,
            etd_minutes: null,
          },
        },
      },
    });
    renderStreamer();
    await waitFor(() => expect(screen.getByTestId("run-estimate").textContent).toBe("预估 —"));
  });
});
