/**
 * 存档页钉团：
 * - 页面三态纪律（T1）：loading / error / empty 三种具名知识态，禁止混用；
 * - 已知面渲染：存活天数 / 周健康四数 / 里程碑日历 DOM 钉（数字全部来自服务端
 *   archive.rs 成文，前端不臆造）；未知行「未就位」措辞与「缺乏起始锚点」逐字钉死；
 * - 导航口径（裁决）：主导航「存档」在位、「图谱」不在；#/graph 原路由仍可达。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

// #/graph 可达性测试会懒加载 GraphPage chunk——cytoscape 抽真件防 jsdom 拉 canvas。
vi.mock("cytoscape", () => ({
  __esModule: true,
  default: () => ({ on: () => {}, destroy: () => {}, $id: () => ({ length: 0, select: () => {} }) }),
}));

import App from "../App";
import { ArchivePage } from "../pages/ArchivePage";
import { RunTrackerProvider } from "../components/RunTracker";
import type { ArchiveView } from "../api";

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

const ROOMS = [{ id: "983", project_name: "演示项目", streamer_uid: "1", output_dir: "o" }];

/** 服务端 fixture 成文快照（= crates/live-server/tests/app_archive.rs 的 full fixture 面）。 */
const KNOWN_ARCHIVE: ArchiveView = {
  alive_days: 66,
  alive_since: "2026-06-01T00:00:00+00:00",
  weekly_health: [
    { key: "repeat_rate", label: "复读率", value_text: "75%（3 次 / 4 人）", known: true },
    { key: "core_danmaku", label: "核心弹幕团", value_text: "名册分档未就位（D2 冻结）", known: false },
    { key: "guard_delta", label: "大航海 delta", value_text: "+3（上轮 22 → 现轮 25）", known: true },
    { key: "follower_delta", label: "涨粉 delta", value_text: "+2（1,000 → 1,002）", known: true },
  ],
  milestones: [
    { key: "full_moon", label: "满月", state: "done", detail_text: "已于 2026-07-01 达成（存活 66 天）" },
    { key: "hundred_days", label: "百天", state: "pending", detail_text: "还差 34 天（存活 66 天）" },
    { key: "thousand_followers", label: "千粉", state: "done", detail_text: "粉丝 1300（目标 1000）达成" },
    { key: "hundred_guards", label: "百舰", state: "pending", detail_text: "还差 75 舰（当前 25）" },
    { key: "anniversary", label: "周年", state: "pending", detail_text: "还差 299 天（存活 66 天）" },
  ],
};

/** 缺存活锚点但周健康有已知行 → 页面照渲，「缺乏起始锚点」逐字钉。 */
const MIXED_NO_ANCHOR: ArchiveView = {
  alive_days: null,
  alive_since: null,
  weekly_health: [
    { key: "repeat_rate", label: "复读率", value_text: "75%（3 次 / 4 人）", known: true },
    { key: "core_danmaku", label: "核心弹幕团", value_text: "名册分档未就位（D2 冻结）", known: false },
    { key: "guard_delta", label: "大航海 delta", value_text: "上轮舰长数据未就位", known: false },
    { key: "follower_delta", label: "涨粉 delta", value_text: "快照未就位（刚建账）", known: false },
  ],
  milestones: [
    { key: "full_moon", label: "满月", state: "unknown", detail_text: "起始锚点未就位" },
    { key: "hundred_days", label: "百天", state: "unknown", detail_text: "起始锚点未就位" },
    { key: "thousand_followers", label: "千粉", state: "unknown", detail_text: "粉丝数未就位" },
    { key: "hundred_guards", label: "百舰", state: "unknown", detail_text: "舰长数未就位" },
    { key: "anniversary", label: "周年", state: "unknown", detail_text: "起始锚点未就位" },
  ],
};

/** 整面全未就位 → 页面落空态（empty 是 T1 具名知识态，不糊整版「未就位」）。 */
const ALL_UNKNOWN: ArchiveView = {
  alive_days: null,
  alive_since: null,
  weekly_health: [
    { key: "repeat_rate", label: "复读率", value_text: "复读率未就位", known: false },
    { key: "core_danmaku", label: "核心弹幕团", value_text: "名册分档未就位（D2 冻结）", known: false },
    { key: "guard_delta", label: "大航海 delta", value_text: "上轮舰长数据未就位", known: false },
    { key: "follower_delta", label: "涨粉 delta", value_text: "快照未就位（刚建账）", known: false },
  ],
  milestones: [
    { key: "full_moon", label: "满月", state: "unknown", detail_text: "起始锚点未就位" },
    { key: "hundred_days", label: "百天", state: "unknown", detail_text: "起始锚点未就位" },
    { key: "thousand_followers", label: "千粉", state: "unknown", detail_text: "粉丝数未就位" },
    { key: "hundred_guards", label: "百舰", state: "unknown", detail_text: "舰长数未就位" },
    { key: "anniversary", label: "周年", state: "unknown", detail_text: "起始锚点未就位" },
  ],
};

function renderArchive(stub: { status: number; body: unknown }) {
  stubFetchMap({ archive: stub });
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <ArchivePage />
    </QueryClientProvider>,
  );
}

function renderApp() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <RunTrackerProvider>
        <App />
      </RunTrackerProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  window.location.hash = "";
});

describe("存档页三态纪律与已知面（R2 批6 D11）", () => {
  it("已知面整体渲染：存活天数 / 周健康四数 / 里程碑日历", async () => {
    renderArchive({ status: 200, body: KNOWN_ARCHIVE });
    // 头注：存活起点口径必须留痕（锚 = 四种既存工件任一的最早时间）。
    await waitFor(() => expect(screen.getByTestId("archive-head-note")).toBeTruthy());

    // 存活天数：数字自豪位 + 起始日（ISO 截取到日期，不臆造更细粒度）。
    const alive = screen.getByTestId("archive-alive");
    expect(alive.textContent).toContain("66");
    expect(alive.textContent).toContain("起始于 2026-06-01");

    // 周健康四数：已知行数字成文，未知行「未就位」措辞。
    expect(screen.getByTestId("health-repeat_rate").textContent).toContain("75%（3 次 / 4 人）");
    expect(screen.getByTestId("health-core_danmaku").textContent).toBe(
      "名册分档未就位（D2 冻结）",
    );
    expect(screen.getByTestId("health-guard_delta").textContent).toBe("+3（上轮 22 → 现轮 25）");
    expect(screen.getByTestId("health-follower_delta").textContent).toBe("+2（1,000 → 1,002）");
    // 未知行落 muted（降强调以示非实据）。
    expect(screen.getByTestId("health-core_danmaku").className).toContain("muted");
    expect(screen.getByTestId("health-repeat_rate").className).not.toContain("muted");

    // 里程碑日历：状态机 done/pending/unknown 三态 + detail 成文。
    expect(screen.getByTestId("milestone-full_moon").getAttribute("data-state")).toBe("done");
    expect(screen.getByTestId("milestone-full_moon").textContent).toContain("已于 2026-07-01 达成");
    expect(screen.getByTestId("milestone-hundred_days").getAttribute("data-state")).toBe("pending");
    expect(screen.getByTestId("milestone-hundred_days").textContent).toContain("还差 34 天");
    expect(screen.getByTestId("milestone-thousand_followers").getAttribute("data-state")).toBe(
      "done",
    );
    expect(screen.getByTestId("milestone-thousand_followers").textContent).toContain(
      "粉丝 1300（目标 1000）达成",
    );
    expect(screen.getByTestId("milestone-hundred_guards").getAttribute("data-state")).toBe(
      "pending",
    );
    expect(screen.getByTestId("milestone-hundred_guards").textContent).toContain("还差 75 舰（当前 25）");
    expect(screen.getByTestId("milestone-anniversary").getAttribute("data-state")).toBe("pending");
    expect(screen.getByTestId("milestone-anniversary").textContent).toContain("还差 299 天");
  });

  it("混态：无锚点但有已知周健康 → 「存活 —（缺乏起始锚点）」，已知行不吞", async () => {
    renderArchive({ status: 200, body: MIXED_NO_ANCHOR });
    const missing = await screen.findByTestId("archive-alive-missing");
    expect(missing.textContent).toContain("存活 —（缺乏起始锚点）");
    // 已知的复读率照渲，不被空态吞掉。
    expect(screen.getByTestId("health-repeat_rate").textContent).toContain("75%");
    expect(screen.queryByTestId("archive-empty")).toBeNull();
    expect(screen.getByTestId("milestone-hundred_guards").getAttribute("data-state")).toBe(
      "unknown",
    );
  });

  it("空态：整面全未就位 → archive-empty 显式空态（不糊整版未就位）", async () => {
    renderArchive({ status: 200, body: ALL_UNKNOWN });
    const empty = await screen.findByTestId("archive-empty");
    expect(empty.textContent).toContain("尚无事实数据");
    expect(screen.queryByTestId("archive-alive")).toBeNull();
    expect(screen.queryByTestId("health-repeat_rate")).toBeNull();
    expect(screen.queryByTestId("archive-milestones")).toBeNull();
  });

  it("错态：服务 500 → notice 错态（T1 具名知识态，不用空态顶替）", async () => {
    renderArchive({ status: 500, body: { error: "存档面失败" } });
    const notice = await screen.findByText(/存档面失败/);
    expect(notice.className).toContain("notice");
  });

  it("载入态：fetch 未决 → state-loading（等待必须看起来在动）", async () => {
    // 永不 resolve 的 fetch → react-query 停留在 pending（isLoading）。
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
    renderArchive({ status: 200, body: ALL_UNKNOWN });
    expect(screen.getByText("载入存档…").className).toContain("state-loading");
  });
});

describe("导航口径（裁决 R2-γ：图谱退主nav、存档承接槽位）", () => {
  it("主导航「存档」在位、「图谱」不在；#/archive 直达存档页", async () => {
    window.location.hash = "#/archive";
    stubFetchMap({
      archive: { status: 200, body: KNOWN_ARCHIVE },
      "rooms/983/overview": {
        status: 200,
        body: { streamer: { name: "演示主播" }, collection: { status: "complete" } },
      },
      "/rooms": { status: 200, body: ROOMS },
    });
    renderApp();

    const nav = screen.getByRole("navigation");
    expect(within(nav).getByText("存档")).toBeTruthy();
    expect(within(nav).queryByText("图谱")).toBeNull();
    // #/archive 实际渲染的是存档页本体。
    await waitFor(() => expect(screen.getByTestId("archive-alive")).toBeTruthy());
  });

  it("#/graph 原路由仍可达（仅退出主导航，图页正常出）", async () => {
    window.location.hash = "#/graph";
    stubFetchMap({
      "rooms/983/graph": { status: 200, body: { elements: [] } },
      "rooms/983/overview": {
        status: 200,
        body: { streamer: { name: "演示主播" }, collection: { status: "complete" } },
      },
      "/rooms": { status: 200, body: ROOMS },
    });
    renderApp();

    await waitFor(() =>
      expect(screen.getByRole("heading", { name: /整体图谱/ })).toBeTruthy(),
    );
    const nav = screen.getByRole("navigation");
    expect(within(nav).queryByText("图谱")).toBeNull();
  });
});