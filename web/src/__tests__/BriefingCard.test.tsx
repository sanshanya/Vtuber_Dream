/**
 * Z5/C1 BriefingCard 三态钉（终裁 P0-5：结论先行 + 句句带出处 + 沉默可呈现）：
 * 1. 未生成 → 空缺位 + 「主播 AI 分析」一键触发（缺席必可见，占位不过界）；
 * 2. 沉默渠（front_brief 缺席/空/形状漂移）→ 显式「证据不足」位（静默与无数据是两态）；
 * 3. 就绪 → 句句带出处、refs 经 episode_index 解析后可点跳个人树、未解析退化为不可点 chip。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BriefingCard, parseBrief } from "../components/BriefingCard";
import { RunTrackerProvider } from "../components/RunTracker";

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
      return { ok: false, status: 404, text: async () => "{}" } as Response;
    }),
  );
}

const VIEWERS = [{ uid: "u1", name: "观众甲", face: null, guard_level: null, medal_level: null, collected_at: null, ai_status: "complete", ai_completed: true, ai_stale: false }];

function mount(props: Parameters<typeof BriefingCard>[0], episodeIndex: unknown = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  stubFetchMap({
    "/viewers": { status: 200, body: VIEWERS },
    "/overview": { status: 200, body: { episode_index: episodeIndex } },
  });
  render(
    <QueryClientProvider client={queryClient}>
      <RunTrackerProvider>
        <BriefingCard {...props} />
      </RunTrackerProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("parseBrief 护栏（ag4-F6：LLM 形状漂移当沉默，不抛）", () => {
  it("front_brief 缺席 / sentences 非数组 → null（沉默渠）", () => {
    expect(parseBrief(undefined)).toBeNull();
    expect(parseBrief({})).toBeNull();
    expect(parseBrief({ front_brief: { sentences: "nope" } })).toBeNull();
  });

  it("空 text 或空 refs 的句子 → 整体漂移判 null（宁缺毋假句）", () => {
    expect(
      parseBrief({ front_brief: { sentences: [{ text: " ", episode_refs: ["e1"] }] } }),
    ).toBeNull();
    expect(
      parseBrief({ front_brief: { sentences: [{ text: "有句", episode_refs: [] }] } }),
    ).toBeNull();
  });

  it("合规句保留；乱形 coverage 剥为 null", () => {
    const parsed = parseBrief({
      front_brief: {
        sentences: [
          { text: "结论", episode_refs: ["e1"], coverage_time_range: ["2026-08-01", "2026-08-04"] },
          { text: "无时段句", episode_refs: ["e2"], coverage_time_range: "8月" },
        ],
      },
    });
    expect(parsed?.length).toBe(2);
    expect(parsed?.[0].range).toEqual(["2026-08-01", "2026-08-04"]);
    expect(parsed?.[1].range).toBeNull();
  });
});

describe("BriefingCard 三态", () => {
  it("未生成：空缺位 + 一键「主播 AI 分析」触发钮", () => {
    mount({ roomId: "1", analysis: undefined, situationStatus: "failed", aiCompletedAt: null, stale: false });
    expect(screen.getByTestId("briefing-empty-slot").textContent).toContain("简报空缺");
    expect(screen.getByRole("button").textContent).toContain("主播 AI 分析");
  });

  it("沉默渠：complete 而无 front_brief → 显式证据不足位，不冒充实况", () => {
    mount({ roomId: "1", analysis: { executive_summary: "x" }, situationStatus: "complete", aiCompletedAt: "2026-08-04T19:32:49Z", stale: false });
    expect(screen.getByTestId("briefing-silent-slot").textContent).toContain("证据不足以成简报");
    expect(screen.getByTestId("briefing-timestamp").textContent).toContain("生成于");
    expect(screen.queryByTestId("briefing-list")).toBeNull();
  });

  it("就绪：refs 经 episode_index 可点跳个人树；未解析 ref 退化为不可点 chip；stale 盖章", async () => {
    mount(
      {
        roomId: "1",
        situationStatus: "complete",
        aiCompletedAt: "2026-08-04T19:32:49Z",
        stale: true,
        analysis: {
          front_brief: {
            sentences: [
              { text: "观众甲围绕《异环》持续升温。", episode_refs: ["ep-1", "ep-ghost"], coverage_time_range: ["2026-08-01", "2026-08-04"] },
            ],
          },
        },
      },
      { "ep-1": { viewer_id: "u1", title: "异环实况" } },
    );
    const links = await screen.findAllByTestId("brief-ref");
    expect(links[0].getAttribute("href")).toBe("#/viewers/u1/tree");
    expect(links[0].textContent).toBe("异环实况");
    expect(screen.getByTestId("brief-ref-unresolved").textContent).toBe("ep-ghost");
    expect(screen.getByTestId("brief-range").textContent).toContain("2026-08-01");
    expect(screen.getByTestId("briefing-stale")).toBeTruthy();
  });
});
