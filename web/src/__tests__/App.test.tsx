/**
 * App 壳层钉团：
 * - RUN_KIND_LABELS 六字面键集钉死（与 registry.rs RUN_KINDS/RUN_KINDS_STAGED 同源冻结，
 *   加 kind 或改文案必须先过此钉）；
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RUN_KIND_LABELS } from "../api";
import App from "../App";
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

