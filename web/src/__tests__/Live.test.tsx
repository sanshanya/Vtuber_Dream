/**
 * 直播数据页钉团（Z4）：
 * - live 面缺档 → 「档案面未建立」+ 空态解说（不臆造对比）；
 * - records 空 → live-empty 空态；
 * - 最后一场 + 7 天内场次 → 「上周均值」卡呈算术平均（1 场样本口径直陈）；
 * - 周窗未含旧场（>7 天）→ 不纳入均值（对比未开句式）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Live } from "../pages/Live";

function stubFetch(status: number, body: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: status < 300, status, text: async () => body }) as Response),
  );
}

function renderLive() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <Live roomId="983" />
    </QueryClientProvider>,
  );
}

/** 固定锚：最后一场 2026-08-02 20:00（unix 秒）。 */
const T_LAST = 1785672000;
// 7 天内一场：2026-07-30 20:00，时长 120 分 → 上周均值钉 = 2 小时 0 分。
const T_WEEK = 1785405000;
// 8 天前一场：不得纳入周窗。
const T_OLD = 1785060000;

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Live 直播数据页（Z4）", () => {
  it("live 面缺档 → 档案面未建立 + 空态解说", async () => {
    stubFetch(200, JSON.stringify({ live: null }));
    renderLive();
    await waitFor(() => expect(screen.getByText(/档案面未建立/)).toBeTruthy());
    expect(screen.getByTestId("live-empty")).toBeTruthy();
  });

  it("records 空（status=empty）→ 空态解说含对比开启条件", async () => {
    stubFetch(
      200,
      JSON.stringify({ live: { status: "empty", count: 0, records: [] } }),
    );
    renderLive();
    await waitFor(() => expect(screen.getByTestId("live-empty")).toBeTruthy());
    expect(screen.getByText(/≥2 场后开启/)).toBeTruthy();
  });

  it("两场且一场入周窗 → 上周均值卡 = 该场时长（样本口径直陈）", async () => {
    stubFetch(
      200,
      JSON.stringify({
        live: {
          status: "ok",
          count: 2,
          records: [
            { title: "第N场", start_time: T_LAST, end_time: T_LAST + 3 * 3600, area_name: "虚拟·日常" },
            { title: "第N-1场", start_time: T_WEEK, end_time: T_WEEK + 2 * 3600 },
          ],
        },
      }),
    );
    renderLive();
    await waitFor(() => expect(screen.getByTestId("live-last")).toBeTruthy());
    const last = screen.getByTestId("live-last");
    expect(last.textContent).toContain("第N场");
    expect(last.textContent).toContain("3 小时 0 分");
    const avg = screen.getByTestId("live-week-avg");
    expect(avg.textContent).toContain("2 小时 0 分");
    expect(avg.textContent).toContain("1 场样本");
    // 场次表全量呈现（降序：最后一场在上）。
    const tableRows = document.querySelectorAll("table.data-table tbody tr");
    expect(tableRows.length).toBe(2);
    expect(tableRows[0].textContent).toContain("第N场");
  });

  it("仅有超过 7 天的旧场 → 对比未开，不臆造均值", async () => {
    stubFetch(
      200,
      JSON.stringify({
        live: {
          status: "ok",
          count: 2,
          records: [
            { title: "第N场", start_time: T_LAST, end_time: T_LAST + 3600 },
            { title: "八天前", start_time: T_OLD, end_time: T_OLD + 6 * 3600 },
          ],
        },
      }),
    );
    renderLive();
    await waitFor(() => expect(screen.getByTestId("live-week-avg")).toBeTruthy());
    expect(screen.getByTestId("live-week-avg").textContent).toContain("0 场样本");
    expect(screen.getByText(/对比未开/)).toBeTruthy();
  });
});
