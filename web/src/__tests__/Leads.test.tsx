/**
 * G2-B（工作项 2）线索页审批钮钉团：
 * - 击「批准」→ POST 审批缝 URL（/api/rooms/{room}/leads/{lead_id}/approve）；
 *   成功后 overview 查询失效重取（待审行从盘面撤退）。
 * - 404 / 422 分支：就地 danger 徽标透传服务端错文。
 * - 标题行：L1 自治位可读（overview leads.autonomy）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Leads } from "../pages/Leads";

interface FetchCall {
  method: string;
  path: string;
}

interface FetchPlan {
  [path: string]: Array<{ status: number; body: string }>;
}

function stubFetchPlan(plan: FetchPlan, calls: FetchCall[]) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      calls.push({ method: init?.method ?? "GET", path });
      const queue = plan[path];
      const next =
        queue && queue.length > 0
          ? queue.shift()!
          : { status: 404, body: JSON.stringify({ error: `未存根的请求：${path}` }) };
      return {
        ok: next.status >= 200 && next.status < 300,
        status: next.status,
        text: async () => next.body,
      } as Response;
    }),
  );
}

function renderLeads(calls: FetchCall[]) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <Leads roomId="983" />
    </QueryClientProvider>,
  );
  return calls;
}

const OVERVIEW_URL = "/api/rooms/983/overview";
const APPROVE_URL = "/api/rooms/983/leads/abc123def4567890/approve";

function overviewBody(over: Record<string, unknown>) {
  return JSON.stringify({
    room_id: "983",
    streamer_uid: "9001",
    project_name: "t",
    streamer: null,
    live: null,
    graph_stats: null,
    collection: { status: "complete", leads_consumed: 0 },
    ai: null,
    situation: null,
    delta: { baseline_only: true },
    ...over,
  });
}

const PENDING = [
  {
    dedupe_key: "abc123def4567890",
    type: "creator",
    locator: "3001",
    viewer_id: "audience",
    motivation: "G2 冒烟线索",
    priority: "medium",
  },
];

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("线索账本审批钮（G2-B）", () => {
  it("击「批准」→ 调审批缝 URL；成功后 overview 重取经，待审行撤退", async () => {
    const calls: FetchCall[] = [];
    stubFetchPlan(
      {
        [OVERVIEW_URL]: [
          {
            status: 200,
            body: overviewBody({
              leads: {
                summary: "[lead_ledger] pending=1 approved=0 consumed=0 rejected=0 deferred=0 by_type={creator: 1} yield_total=0 latest_consumed=[]",
                totals: { pending_approval: 1, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
                autonomy: 1,
                pending: PENDING,
              },
            }),
          },
          {
            status: 200,
            body: overviewBody({
              leads: {
                summary: "[lead_ledger] pending=0 approved=1 consumed=0 rejected=0 deferred=0 by_type={creator: 1} yield_total=0 latest_consumed=[]",
                totals: { pending_approval: 0, approved: 1, consumed: 0, rejected: 0, deferred: 0 },
                autonomy: 1,
                pending: [],
              },
            }),
          },
        ],
        [APPROVE_URL]: [
          { status: 200, body: JSON.stringify({ dedupe_key: "abc123def4567890", status: "approved", changed: true }) },
        ],
      },
      calls,
    );
    renderLeads(calls);

    // 待审行渲染 + 标题行 L1 自治位可读（autonomy=1 → 开）
    const button = await screen.findByTestId("lead-approve-abc123def4567890");
    expect(screen.getByTestId("leads-autonomy").textContent).toContain("L1");
    expect(screen.getByTestId("leads-autonomy").textContent).toContain("开");

    fireEvent.click(button);
    await waitFor(() =>
      expect(
        calls.some((c) => c.method === "POST" && c.path === APPROVE_URL),
      ).toBe(true),
    );
    // 成功后 overview 失效重取：第二次 GET overview 必须发生，待审行撤退
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "GET" && c.path === OVERVIEW_URL).length).toBe(2),
    );
    await waitFor(() =>
      expect(screen.queryByTestId("lead-approve-abc123def4567890")).toBeNull(),
    );
  });

  it("404 分支：审批缝回不存在 → 就地 danger 透传错文", async () => {
    const calls: FetchCall[] = [];
    stubFetchPlan(
      {
        [OVERVIEW_URL]: [
          {
            status: 200,
            body: overviewBody({
              leads: {
                totals: { pending_approval: 1 },
                autonomy: 0,
                pending: PENDING,
              },
            }),
          },
        ],
        [APPROVE_URL]: [{ status: 404, body: JSON.stringify({ error: "lead abc123def4567890 不存在" }) }],
      },
      calls,
    );
    renderLeads(calls);
    fireEvent.click(await screen.findByTestId("lead-approve-abc123def4567890"));
    await waitFor(() =>
      expect(screen.getByTestId("lead-approve-error").textContent).toContain(
        "lead abc123def4567890 不存在",
      ),
    );
    // L1 关时标题行亦可读
    expect(screen.getByTestId("leads-autonomy").textContent).toContain("关");
  });

  it("422 分支：非法迁移 → 就地 danger 透传状态机规则文案", async () => {
    const calls: FetchCall[] = [];
    stubFetchPlan(
      {
        [OVERVIEW_URL]: [
          {
            status: 200,
            body: overviewBody({
              leads: {
                totals: { pending_approval: 1 },
                autonomy: 0,
                pending: PENDING,
              },
            }),
          },
        ],
        [APPROVE_URL]: [
          {
            status: 422,
            body: JSON.stringify({
              error: "状态机只许 pending_approval → approved；当前状态 consumed，不允许此迁移",
            }),
          },
        ],
      },
      calls,
    );
    renderLeads(calls);
    fireEvent.click(await screen.findByTestId("lead-approve-abc123def4567890"));
    await waitFor(() =>
      expect(screen.getByTestId("lead-approve-error").textContent).toContain("状态机只许"),
    );
  });
});
