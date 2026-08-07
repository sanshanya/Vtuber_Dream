/**
 * App 壳层钉团：
 * - RUN_KIND_LABELS 六字面键集钉死（与 registry.rs RUN_KINDS/RUN_KINDS_STAGED 同源冻结，
 *   加 kind 或改文案必须先过此钉）；
 * - footer synthetic 徽标 = isSyntheticRun 析取口径（collection/ai/situation 任一
 *   synthetic_demo=true → 亮；三分段全缺席/全 false → 不亮，合成标示宁可缺席、不许臆造）——
 *   纯函数钉 + footer 渲染钉两面。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RUN_KIND_LABELS } from "../api";
import App, { isSyntheticRun } from "../App";
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
      return { ok: false, status: 404, text: async () => JSON.stringify({ error: "?" }) } as Response;
    }),
  );
}

const ROOMS = [{ id: "983", project_name: "演示项目", streamer_uid: "1", output_dir: "o" }];

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
});

describe("RUN_KIND_LABELS 字面键集（R3#5）", () => {
  it("六字面键与文案钉死（registry.rs 同源）", () => {
    expect(RUN_KIND_LABELS).toEqual({
      full: "全量感知",
      viewer: "单舰长感知",
      collect_streamer: "主播采集",
      collect_guards: "舰长采集",
      ai_viewers: "舰长 AI 分析",
      ai_audience: "主播 AI 分析",
    });
  });
});

describe("isSyntheticRun 析取口径（R3#6 单源）", () => {
  it("collection/ai/situation 任一分段 synthetic_demo=true → 亮", () => {
    expect(isSyntheticRun({ collection: { synthetic_demo: true } })).toBe(true);
    expect(isSyntheticRun({ ai: { synthetic_demo: true } })).toBe(true);
    expect(isSyntheticRun({ situation: { synthetic_demo: true } })).toBe(true);
  });

  it("三分段全缺席 / 全 false / 无 overview → 不亮（不凭失败臆造）", () => {
    expect(isSyntheticRun(undefined)).toBe(false);
    expect(isSyntheticRun({})).toBe(false);
    expect(
      isSyntheticRun({
        collection: { synthetic_demo: false },
        ai: { synthetic_demo: false },
        situation: { synthetic_demo: false },
      }),
    ).toBe(false);
  });
});

describe("App footer synthetic 徽标（W2/r5-F3 渲染面）", () => {
  it("overview 任一分段 synthetic → footer 亮全局徽标", async () => {
    stubFetchMap({
      overview: {
        status: 200,
        body: { collection: { status: "complete" }, ai: { synthetic_demo: true } },
      },
      viewers: { status: 200, body: [] },
      "/rooms": { status: 200, body: ROOMS },
    });
    renderApp();
    await waitFor(() => expect(screen.getByTestId("app-synthetic")).toBeTruthy());
  });

  it("三分段全缺席 → footer 不亮", async () => {
    stubFetchMap({
      overview: {
        status: 200,
        body: { collection: { status: "complete" }, streamer: { name: "演示主播" } },
      },
      viewers: { status: 200, body: [] },
      "/rooms": { status: 200, body: ROOMS },
    });
    renderApp();
    // 先等数据面真正落地（房间名回填 = overview 已消费），再断言缺席。
    await waitFor(() => expect(screen.getByTestId("room-current").textContent).toBe("演示主播"));
    expect(screen.queryByTestId("app-synthetic")).toBeNull();
  });
});
