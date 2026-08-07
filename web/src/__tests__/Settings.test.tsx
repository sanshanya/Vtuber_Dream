/**
 * 设置页钉团（白名单第 5 键）：
 * - 白名单第 5 键 run_budget_cny：输入框在场、PUT /api/config 请求体随行、空串不炸；
 * - 可写键清单五键徽标（含 ai.run_budget_cny）；
 * - 月度实耗行（/api/budget）：本月已耗/次数/单次预算/最近一次；null 预算 → 「未设」。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Settings } from "../pages/Settings";

const CONFIG = {
  project_name: "虚梦验收",
  output_dir: "/tmp/out",
  bilibili: {
    room_id: "1",
    streamer_uid: "983",
    cookie_present: true,
    additional_viewer_ids: [],
  },
  ai: {
    api: "chat_completions",
    base_url: "http://llm.local/v1",
    model: "deepseek-chat",
    api_key_present: true,
    run_budget_cny: 4.0,
  },
  writable_keys: [
    "bilibili.cookie",
    "ai.api_key",
    "ai.base_url",
    "ai.model",
    "ai.run_budget_cny",
  ],
};

const BUDGET = {
  budget_cny: 4.0,
  month: "2026-08",
  month_cost_cny: 0.35,
  month_runs: 3,
  last_run: {
    run_id: "r-1",
    ts: "2026-08-05T00:00:00+00:00",
    cost_cny: 3.0,
    status: "failed",
    kind: "full",
    spend_mode: "normal",
  },
};

interface Call {
  url: string;
  method: string;
  body: unknown;
}

function stubSettingsFetch(config: unknown, budget: unknown) {
  const calls: Call[] = [];
  const mock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = (init?.method ?? "GET").toUpperCase();
    calls.push({ url, method, body: init?.body === undefined ? undefined : JSON.parse(String(init.body)) });
    const text = async (status: number, body: unknown) =>
      ({ ok: status < 300, status, text: async () => JSON.stringify(body) }) as Response;
    if (url === "/api/budget") return text(200, budget);
    if (url === "/api/config") {
      if (method === "PUT") return text(200, { status: "updated", keys: 5 });
      return text(200, config);
    }
    return text(404, { error: "?" });
  });
  vi.stubGlobal("fetch", mock);
  return { mock, calls };
}

function renderSettings() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <Settings />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Settings 白名单五键（Z6 件5）", () => {
  it("可写键清单含第 5 键 ai.run_budget_cny，且预算输入框在场", async () => {
    stubSettingsFetch(CONFIG, BUDGET);
    renderSettings();
    await waitFor(() => expect(screen.getByText("ai.run_budget_cny")).toBeTruthy());
    for (const key of CONFIG.writable_keys) {
      expect(screen.getByText(key)).toBeTruthy();
    }
    const input = screen.getByLabelText(/AI 单次预算/) as HTMLInputElement;
    expect(input).toBeTruthy();
    // 现值回显在 placeholder（null=未设限;有值 → 数字串）。
    expect((input.placeholder as string | undefined) ?? "").toBe("4");
  });

  it("填写预算并保存 → PUT /api/config 请求体 run_budget_cny 随行", async () => {
    const { calls } = stubSettingsFetch(CONFIG, BUDGET);
    renderSettings();
    await waitFor(() => expect(screen.getByLabelText(/AI 单次预算/)).toBeTruthy());
    fireEvent.change(screen.getByLabelText(/AI 单次预算/), { target: { value: "2.00" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    await waitFor(() => expect(screen.getByText(/已写入 5 个键/)).toBeTruthy());
    const put = calls.find((call) => call.method === "PUT" && call.url === "/api/config");
    expect(put).toBeDefined();
    const ai = (put?.body as { ai?: Record<string, unknown> } | undefined)?.ai;
    expect(ai?.run_budget_cny).toBe("2.00");
  });

  it("月度实耗行渲染 /api/budget 各段（已耗/次数/单次预算/最近一次）", async () => {
    stubSettingsFetch(CONFIG, BUDGET);
    renderSettings();
    await waitFor(() => expect(screen.getByTestId("monthly-budget")).toBeTruthy());
    const line = screen.getByTestId("monthly-budget").textContent ?? "";
    expect(line).toContain("2026-08");
    expect(line).toContain("¥0.35");
    expect(line).toContain("3 次运行");
    expect(line).toContain("¥4.00");
    expect(line).toContain("最近一次：full（failed）≈¥3.00");
  });

  it("预算 null（未设闸）与空历史 → 「未设」+「暂无历史记录」，不臆造数字", async () => {
    stubSettingsFetch(
      { ...CONFIG, ai: { ...CONFIG.ai, run_budget_cny: null } },
      { budget_cny: null, month: "2026-08", month_cost_cny: 0, month_runs: 0, last_run: null },
    );
    renderSettings();
    await waitFor(() => expect(screen.getByTestId("monthly-budget")).toBeTruthy());
    const line = screen.getByTestId("monthly-budget").textContent ?? "";
    expect(line).toContain("单次预算 未设");
    expect(line).toContain("暂无历史记录");
  });
});