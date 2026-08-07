/**
 * 预算阻断卡钉团（Z6 件2/件5 前端行动面）：
 * - outcome.budget_block → 卡片渲染（预估/预算/新鲜比/hint verbatim + 去设置页链接）；
 * - 「只跑增量」/「只推简报」→ 保留 kind/viewer_uid + spend_mode 随行重发 POST；
 * - ai_audience 无单人感知段 → 不摆两选钮（服务端 422 面），只留链路；
 * - 无 budget_block 的 failed → 不渲染卡。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { type RunRecordView } from "../api";
import { RunStatusBadge } from "../components/RunButton";
import { RunTrackerProvider, useRunTracker } from "../components/RunTracker";

function blockedOutcome(spend_mode = "normal") {
  return {
    error: "预估超预算",
    budget_block: {
      spend_mode,
      estimated_cny: 3.0,
      budget_cny: 0.01,
      fresh_viewers: 1,
      total_viewers: 1,
      hint: "两选重发：spend_mode=incremental 只更新变化者 / briefing_only 只推简报",
    },
  };
}

function makeBlockedRecord(over: Partial<RunRecordView> = {}): RunRecordView {
  return {
    run_id: "r-1",
    kind: "full",
    viewer_uid: null,
    force: false,
    status: "failed",
    started_at: "2026-08-05T00:00:00+00:00",
    finished_at: "2026-08-05T00:00:10+00:00",
    partial: false,
    outcome: blockedOutcome(),
    events: [],
    ...over,
  };
}

interface Call {
  url: string;
  method: string;
  body: unknown;
}

function harness(record: RunRecordView) {
  const calls: Call[] = [];
  const mock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = (init?.method ?? "GET").toUpperCase();
    calls.push({ url, method, body: init?.body === undefined ? undefined : JSON.parse(String(init.body)) });
    const text = async (status: number, body: unknown) =>
      ({ ok: status < 300, status, text: async () => JSON.stringify(body) }) as Response;
    if (method === "POST" && url === "/api/runs") return text(202, { run_id: "r-2" });
    if (url === "/api/runs/r-1") return text(200, record);
    if (url === "/api/runs/r-2")
      return text(200, { ...record, run_id: "r-2", status: "done", outcome: null });
    return text(404, { error: "?" });
  });
  vi.stubGlobal("fetch", mock);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  function Probe() {
    const tracker = useRunTracker();
    return (
      <div>
        <button onClick={() => tracker.track("r-1")}>track</button>
        <RunStatusBadge />
      </div>
    );
  }
  render(
    <QueryClientProvider client={queryClient}>
      <RunTrackerProvider>
        <Probe />
      </RunTrackerProvider>
    </QueryClientProvider>,
  );
  return { mock, calls };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("预算阻断卡（outcome.budget_block）", () => {
  it("failed + budget_block → 卡片渲染数字面与 hint verbatim，带去设置页链接", async () => {
    harness(makeBlockedRecord());
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByTestId("budget-block")).toBeTruthy());
    const card = screen.getByTestId("budget-block");
    expect(card.textContent).toContain("¥3.00"); // estimated 3.0
    expect(card.textContent).toContain("¥0.01"); // budget 0.01
    expect(card.textContent).toContain("1/1"); // fresh/total
    expect(card.textContent).toContain(
      "两选重发：spend_mode=incremental 只更新变化者 / briefing_only 只推简报",
    );
    const link = screen.getByRole("link", { name: "去设置页改预算" });
    expect(link.getAttribute("href")).toBe("#/settings");
  });

  it("full 无 viewer_uid → 「只跑增量」重发体 {kind, spend_mode} 不带 viewer_uid", async () => {
    const { calls } = harness(makeBlockedRecord());
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByTestId("budget-block")).toBeTruthy());
    fireEvent.click(screen.getByTestId("budget-retry-incremental"));
    await waitFor(() =>
      expect(calls.some((call) => call.method === "POST" && call.url === "/api/runs")).toBe(true),
    );
    const post = calls.find((call) => call.method === "POST" && call.url === "/api/runs");
    expect(post?.body).toEqual({ kind: "full", spend_mode: "incremental" });
  });

  it("viewer 保留 viewer_uid → 「只推简报」重发体含 uid + spend_mode", async () => {
    const { calls } = harness(
      makeBlockedRecord({ kind: "viewer", viewer_uid: "1003" }),
    );
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByTestId("budget-block")).toBeTruthy());
    fireEvent.click(screen.getByTestId("budget-retry-briefing"));
    await waitFor(() =>
      expect(calls.some((call) => call.method === "POST" && call.url === "/api/runs")).toBe(true),
    );
    const post = calls.find((call) => call.method === "POST" && call.url === "/api/runs");
    expect(post?.body).toEqual({ kind: "viewer", viewer_uid: "1003", spend_mode: "briefing_only" });
  });

  it("ai_audience 无单人感知段 → 卡片仍渲染但不摆两选钮，只留改预算链路", async () => {
    harness(makeBlockedRecord({ kind: "ai_audience" }));
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByTestId("budget-block")).toBeTruthy());
    expect(screen.queryByTestId("budget-retry-incremental")).toBeNull();
    expect(screen.queryByTestId("budget-retry-briefing")).toBeNull();
    expect(screen.getByRole("link", { name: "去设置页改预算" })).toBeTruthy();
  });

  it("无 budget_block 的 failed → 不渲染卡", async () => {
    harness(makeBlockedRecord({ outcome: { error: "采集 404" } }));
    fireEvent.click(screen.getByText("track"));
    await waitFor(() => expect(screen.getByText("采集 404")).toBeTruthy());
    expect(screen.queryByTestId("budget-block")).toBeNull();
  });
});