/**
 * 直播数据页钉团：
 * - live 面缺档 → 「档案面未建立」+ 空态解说（不臆造对比）；
 * - records 空 → live-empty 空态；
 * - 最后一场 + 7 天内场次 → 「上周均值」卡呈算术平均（1 场样本口径直陈）；
 * - 周窗未含旧场（>7 天）→ 不纳入均值（对比未开句式）。
 * 追加钉团：
 * - status 汉化三态（ok→正常 / error→接口故障 / 缺省→—）；
 * - error 分支独立错文并透传 errors 字段（不再撞「主播暂无回放列表」空态）；
 * - 指标列落地（观看/弹幕/在线）：记录真实携带才渲值，缺席 → —。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RunTrackerProvider } from "../components/RunTracker";
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
      {/* 页面动作栏含 KindRunButton，需 RunTracker 上下文。 */}
      <RunTrackerProvider>
        <Live roomId="983" />
      </RunTrackerProvider>
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

describe("自动采录开关", () => {
  it("缺档 → OFF 呈现 + 省 token 提示（不臆造开关位）", async () => {
    stubFetch(200, JSON.stringify({ live: null, ws_windows: null, auto_collect: null }));
    renderLive();
    await waitFor(() => {
      const btn = screen.getByTestId("auto-collect-toggle");
      expect(btn.textContent).toBe("自动采录：关");
      expect(btn.getAttribute("aria-pressed")).toBe("false");
    });
    expect(screen.getByTestId("auto-collect-hint").textContent).toContain("省 token");
  });

  it("显 true → ON 呈现 + aria-pressed", async () => {
    stubFetch(
      200,
      JSON.stringify({ live: null, ws_windows: null, auto_collect: { enabled: true } }),
    );
    renderLive();
    await waitFor(() => {
      const btn = screen.getByTestId("auto-collect-toggle");
      expect(btn.textContent).toBe("自动采录：开");
      expect(btn.getAttribute("aria-pressed")).toBe("true");
    });
  });

  it("点击 → POST 正确载荷 + 翻转到新终态（overview 重取后自成）", async () => {
    let enabledNow = false;
    const calls: Array<{ method?: string; url: string; body?: string }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        calls.push({ method: init?.method, url, body: init?.body as string | undefined });
        if (url.includes("auto-collect")) {
          const parsed = JSON.parse(String(init?.body)) as { enabled: boolean };
          enabledNow = parsed.enabled;
          return {
            ok: true,
            status: 200,
            text: async () => JSON.stringify({ enabled: enabledNow, changed: true }),
          } as Response;
        }
        return {
          ok: true,
          status: 200,
          text: async () =>
            JSON.stringify({ live: null, ws_windows: null, auto_collect: { enabled: enabledNow } }),
        } as Response;
      }),
    );
    renderLive();
    await waitFor(() => expect(screen.getByTestId("auto-collect-toggle").textContent).toBe("自动采录：关"));
    screen.getByTestId("auto-collect-toggle").click();
    await waitFor(() =>
      expect(screen.getByTestId("auto-collect-toggle").textContent).toBe("自动采录：开"),
    );
    const post = calls.find((c) => c.method === "POST" && c.url.includes("auto-collect"));
    expect(post).toBeTruthy();
    expect(post!.body).toBe(JSON.stringify({ enabled: true }));
  });
});

describe("采录场次窗（WS 实采卡）", () => {
  it("ws_windows 缺档 → 「尚无采录场次」空态（场均真相不臆造）", async () => {
    stubFetch(200, JSON.stringify({ live: null, ws_windows: null }));
    renderLive();
    await waitFor(() => expect(screen.getByTestId("ws-windows-empty")).toBeTruthy());
  });

  it("两场窗 → 各行真实价（含零付费礼物直白位）", async () => {
    stubFetch(
      200,
      JSON.stringify({
        live: { status: "empty", count: 0, records: [] },
        ws_windows: {
          generated_at: "2026-08-10T04:00:00+00:00",
          windows: [
            {
              session: { start_timestamp: 1785672000, end_timestamp: 1785679200, rid: "ws:1785672000" },
              lines: 123,
              speakers: 7,
              danmaku: 120,
              super_chat: 0,
              money: { paid_gifts: 0, gift_yuan: 0, sc_count: 0, sc_yuan: 0, guard_buys: 0, toasts: 0 },
            },
            {
              session: { start_timestamp: 1785844555, end_timestamp: 1785861737, rid: "ws:1785844555" },
              lines: 294,
              speakers: 20,
              danmaku: 255,
              super_chat: 0,
              money: { paid_gifts: 23, gift_yuan: 33.8, sc_count: 0, sc_yuan: 0, guard_buys: 3, toasts: 16 },
            },
          ],
        },
      }),
    );
    renderLive();
    const table = await waitFor(() => screen.getByTestId("ws-windows-table"));
    expect(table.textContent).toContain("7");
    expect(table.textContent).toContain("120");
    expect(table.textContent).toContain("零付费礼物");
    expect(table.textContent).toContain("¥33.8（23 次）");
    expect(table.textContent).toContain("3 / 16");
    expect(screen.queryByTestId("ws-windows-empty")).toBeNull();
  });
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

describe("FE-F2：Live 三修", () => {
  it("status 汉化：ok → 正常", async () => {
    stubFetch(
      200,
      JSON.stringify({
        live: {
          status: "ok",
          count: 1,
          records: [{ title: "A", start_time: T_LAST, end_time: T_LAST + 3600 }],
        },
      }),
    );
    renderLive();
    await waitFor(() => expect(screen.getByTestId("live-status").textContent).toContain("正常"));
    expect(screen.getByTestId("live-status").textContent).toContain("1 场");
  });

  it("status 缺省/未知 → —", async () => {
    stubFetch(
      200,
      JSON.stringify({
        live: {
          count: 0,
          records: [],
        },
      }),
    );
    renderLive();
    await waitFor(() => {
      const badge = screen.getByTestId("live-status");
      expect(badge.textContent).toContain("—");
      expect(badge.textContent).not.toContain("undefined");
    });
  });

  it("error 分支：独立错文并透传 errors，不再撞「主播暂无回放列表」", async () => {
    stubFetch(
      200,
      JSON.stringify({
        live: {
          status: "error",
          count: 0,
          errors: ["HTTP 412：风控拦截，record/getList 被平台拒绝"],
          records: [],
        },
      }),
    );
    renderLive();
    await waitFor(() => {
      const badge = screen.getByTestId("live-status");
      expect(badge.textContent).toContain("接口故障");
    });
    const notice = screen.getByTestId("live-error");
    expect(notice.textContent).toContain("HTTP 412：风控拦截，record/getList 被平台拒绝");
    expect(notice.textContent).not.toContain("主播暂无回放列表");
    expect(screen.queryByTestId("live-empty")).toBeNull();
  });

  it("指标列落地：记录携带 watch_num/danmu_num/online → 渲值；缺席 → —", async () => {
    stubFetch(
      200,
      JSON.stringify({
        live: {
          status: "ok",
          count: 2,
          records: [
            {
              title: "全场次",
              start_time: T_LAST,
              end_time: T_LAST + 3600,
              watch_num: 1051,
              danmu_num: 233,
              online: 66,
            },
            { title: "裸场次", start_time: T_WEEK, end_time: T_WEEK + 1800 },
          ],
        },
      }),
    );
    renderLive();
    await waitFor(() => expect(screen.getByTestId("live-last")).toBeTruthy());
    const head = Array.from(document.querySelectorAll("table.data-table thead th")).map(
      (th) => th.textContent,
    );
    expect(head).toEqual(["封面", "场次", "开播时间", "时长", "分区", "观看", "弹幕", "在线"]);
    const rows = document.querySelectorAll("table.data-table tbody tr");
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain("1051");
    expect(rows[0].textContent).toContain("233");
    expect(rows[0].textContent).toContain("66");
    const cells = Array.from(rows[1].querySelectorAll("td")).map((td) => td.textContent);
    expect(cells.slice(5)).toEqual(["—", "—", "—"]);
  });
});
