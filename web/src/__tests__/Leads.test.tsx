/**
 * 线索页审批钮钉团：
 * - 击「批准」→ POST 审批缝 URL（/api/rooms/{room}/leads/{lead_id}/approve）；
 *   成功后 overview 查询失效重取（待审行从盘面撤退）。
 * - 404 / 422 分支：就地 danger 徽标透传服务端错文。
 * - 标题行：L1 自治位可读（overview leads.autonomy）。
 * 追加钉团：
 * - leads.summary 的 [lead_ledger] 裸文本行不上墙（人话面 = 五态徽标）；
 * - 空账（五态全零）显式空态一句（leads-empty）；
 * - 在飞期间批准钮禁且双击只发一次 POST（busy 护栏 + disabled 双保险）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Leads } from "../pages/Leads";

interface FetchCall {
  method: string;
  path: string;
  /** POST 体原样（拒因断言用；未带体为空串）。 */
  body?: string;
}

interface FetchPlan {
  [path: string]: Array<{ status: number; body: string }>;
}

function stubFetchPlan(plan: FetchPlan, calls: FetchCall[]) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      calls.push({
        method: init?.method ?? "GET",
        path,
        body: init?.body == null ? "" : String(init.body),
      });
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

describe("FE-F2：LeadsBlock 呈现与在飞护栏", () => {
  it("[lead_ledger] 裸摘要行不上墙——人话面 = 五态徽标", async () => {
    const calls: FetchCall[] = [];
    stubFetchPlan(
      {
        [OVERVIEW_URL]: [
          {
            status: 200,
            body: overviewBody({
              leads: {
                summary:
                  "[lead_ledger] pending=1 approved=0 consumed=0 rejected=0 deferred=0 by_type={creator: 1} yield_total=0 latest_consumed=[]",
                totals: { pending_approval: 1, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
                autonomy: 0,
                pending: PENDING,
              },
            }),
          },
        ],
      },
      calls,
    );
    renderLeads(calls);
    await screen.findByTestId("lead-approve-abc123def4567890");
    expect(screen.queryByText(/lead_ledger/)).toBeNull();
    const badges = screen.getByTestId("leads-totals");
    expect(badges.textContent).toContain("待审批 1");
    expect(badges.textContent).toContain("已批准 0");
    expect(badges.textContent).toContain("暂缓 0");
    expect(screen.queryByTestId("leads-empty")).toBeNull();
  });

  it("空账（五态全零）→ 显式空态一句（leads-empty）", async () => {
    const calls: FetchCall[] = [];
    stubFetchPlan(
      {
        [OVERVIEW_URL]: [
          {
            status: 200,
            body: overviewBody({
              leads: {
                summary:
                  "[lead_ledger] pending=0 approved=0 consumed=0 rejected=0 deferred=0 by_type={} yield_total=0 latest_consumed=[]",
                totals: { pending_approval: 0, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
                autonomy: 0,
                pending: [],
              },
            }),
          },
        ],
      },
      calls,
    );
    renderLeads(calls);
    const empty = await screen.findByTestId("leads-empty");
    expect(empty.textContent).toContain("暂无线索");
    expect(empty.textContent).toContain("可疑方向会落进这里");
    expect(screen.queryByText(/lead_ledger/)).toBeNull();
    expect(screen.getByTestId("leads-totals").textContent).toContain("待审批 0");
  });

  it("在飞期间钮禁且双击只发一次 POST（busy 护栏）", async () => {
    const calls: FetchCall[] = [];
    let releasePost: (() => void) | null = null;
    const overviewOk = () =>
      ({
        ok: true,
        status: 200,
        text: async () =>
          overviewBody({
            leads: {
              totals: { pending_approval: 1, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
              autonomy: 0,
              pending: PENDING,
            },
          }),
      }) as Response;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input);
        calls.push({ method: init?.method ?? "GET", path });
        if (path === APPROVE_URL) {
          await new Promise<void>((resolve) => {
            releasePost = resolve;
          });
          return {
            ok: true,
            status: 200,
            text: async () =>
              JSON.stringify({ dedupe_key: "abc123def4567890", status: "approved", changed: true }),
          } as Response;
        }
        return overviewOk();
      }),
    );
    renderLeads(calls);
    const button = await screen.findByTestId("lead-approve-abc123def4567890");
    fireEvent.click(button);
    // 在飞面：钮禁 + 「批准中…」
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(true));
    expect(button.textContent).toBe("批准中…");
    // 双击/连击：不得撕开第二条 POST
    fireEvent.click(button);
    fireEvent.doubleClick(button);
    fireEvent.click(button);
    releasePost!();
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
    expect(calls.filter((c) => c.method === "POST" && c.path === APPROVE_URL).length).toBe(1);
  });
});

/**
 * 拒绝缝追加钉团：
 * - 行级「拒绝」钮一击即飞、无 dialog（window.confirm 零调用）；全空拒因合法提交
 *   （POST 体 {"chips":[],"note":""} → 服务端 NULL/NULL 留档）。
 * - 行内拒因区 = 四 chip 白名单 + 一条注记；选中后提交 → POST 体携带 chips/note。
 * - pending 按持有人（viewer_id）分组、组默认折叠（<details> 无 open 属性）——
 *   DOM 在场：闭合组内行级钮仍可被查询与一击。
 * - 组级「全批/全拒」前端逐行 fan-out：一股作气、逐行 POST（无批量服务端面）。
 * - rejected 明细直出：徽标即 <details>，展开回看记录 chip/note（只读事实面）。
 */
describe("D9：拒绝面与持有人分组", () => {
  const REJECT_URL = "/api/rooms/983/leads/abc123def4567890/reject";
  // chip 白名单 = 服务端 overview 下发（唯一真源）；fixture 统一带此键——
  // 前端本地已无任何白名单字面。
  const overviewFor = (leads: Record<string, unknown>) => ({
    [OVERVIEW_URL]: [
      {
        status: 200,
        body: overviewBody({
          leads: {
            reject_chip_reasons: ["太泛", "不对路", "已知道", "做不了"],
            ...leads,
          },
        }),
      },
    ],
  });

  it("行级「拒绝」一击即飞、无 dialog；全空拒因合法提交", async () => {
    const calls: FetchCall[] = [];
    const confirmSpy = vi.spyOn(window, "confirm").mockImplementation(() => true);
    stubFetchPlan(
      {
        ...overviewFor({
          totals: { pending_approval: 1, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
          autonomy: 0,
          pending: PENDING,
        }),
        [REJECT_URL]: [
          {
            status: 200,
            body: JSON.stringify({
              dedupe_key: "abc123def4567890",
              status: "rejected",
              changed: true,
              reject_chips: [],
              reject_note: "",
            }),
          },
        ],
      },
      calls,
    );
    renderLeads(calls);
    // 组默认折叠——闭合态其内容仍在 DOM 中，行级钮可查询可一击。
    const group = await screen.findByTestId("lead-group-audience");
    expect(group.hasAttribute("open")).toBe(false);
    fireEvent.click(await screen.findByTestId("lead-reject-abc123def4567890"));
    await waitFor(() =>
      expect(calls.some((c) => c.method === "POST" && c.path === REJECT_URL)).toBe(true),
    );
    // 无 dialog：window.confirm 全程零调用。
    expect(confirmSpy).not.toHaveBeenCalled();
    const post = calls.find((c) => c.method === "POST" && c.path === REJECT_URL)!;
    expect(JSON.parse(post.body!)).toEqual({ chips: [], note: "" });
    confirmSpy.mockRestore();
  });

  it("行内拒因区：chip 白名单 + 注记选中后提交 → POST 体携带", async () => {
    const calls: FetchCall[] = [];
    stubFetchPlan(
      {
        ...overviewFor({
          totals: { pending_approval: 1, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
          autonomy: 0,
          pending: PENDING,
        }),
        [REJECT_URL]: [
          {
            status: 200,
            body: JSON.stringify({
              dedupe_key: "abc123def4567890",
              status: "rejected",
              changed: true,
              reject_chips: ["太泛"],
              reject_note: "主播不玩这品类",
            }),
          },
        ],
      },
      calls,
    );
    renderLeads(calls);
    const chip = await screen.findByTestId("lead-reject-chip-abc123def4567890-太泛");
    fireEvent.click(chip);
    fireEvent.change(screen.getByTestId("lead-reject-note-abc123def4567890"), {
      target: { value: "主播不玩这品类" },
    });
    fireEvent.click(screen.getByTestId("lead-reject-abc123def4567890"));
    await waitFor(() =>
      expect(calls.some((c) => c.method === "POST" && c.path === REJECT_URL)).toBe(true),
    );
    const post = calls.find((c) => c.method === "POST" && c.path === REJECT_URL)!;
    expect(JSON.parse(post.body!)).toEqual({ chips: ["太泛"], note: "主播不玩这品类" });
  });

  it("组级「全批」前端逐行 fan-out：一股作气按行发 POST", async () => {
    const calls: FetchCall[] = [];
    const groupRows = Array.from({ length: 10 }, (_, i) => ({
      dedupe_key: `row-${i}`,
      type: "creator",
      locator: `L-${i}`,
      viewer_id: "audience",
      motivation: `批量第 ${i} 条`,
    }));
    const plan: FetchPlan = overviewFor({
      totals: { pending_approval: 10, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
      autonomy: 0,
      pending: groupRows,
    });
    groupRows.forEach((r) => {
      plan[`/api/rooms/983/leads/${r.dedupe_key}/approve`] = [
        { status: 200, body: JSON.stringify({ dedupe_key: r.dedupe_key, status: "approved", changed: true }) },
      ];
    });
    stubFetchPlan(plan, calls);
    renderLeads(calls);
    fireEvent.click(await screen.findByTestId("lead-approve-all-audience"));
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "POST" && c.path.endsWith("/approve")).length).toBe(10),
    );
  });

  it("组级「全拒」前端逐行 fan-out：每股空拒因（全空合法）", async () => {
    const calls: FetchCall[] = [];
    const groupRows = Array.from({ length: 10 }, (_, i) => ({
      dedupe_key: `row-${i}`,
      type: "creator",
      locator: `L-${i}`,
      viewer_id: "momo",
      motivation: `批量第 ${i} 条`,
    }));
    const plan: FetchPlan = overviewFor({
      totals: { pending_approval: 10, approved: 0, consumed: 0, rejected: 0, deferred: 0 },
      autonomy: 0,
      pending: groupRows,
    });
    groupRows.forEach((r) => {
      plan[`/api/rooms/983/leads/${r.dedupe_key}/reject`] = [
        { status: 200, body: JSON.stringify({ dedupe_key: r.dedupe_key, status: "rejected", changed: true, reject_chips: [], reject_note: "" }) },
      ];
    });
    stubFetchPlan(plan, calls);
    renderLeads(calls);
    fireEvent.click(await screen.findByTestId("lead-reject-all-momo"));
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "POST" && c.path.endsWith("/reject")).length).toBe(10),
    );
    const posts = calls.filter((c) => c.method === "POST" && c.path.endsWith("/reject"));
    expect(posts.every((c) => JSON.parse(c.body!) && JSON.parse(c.body!).chips.length === 0)).toBe(true);
  });

  it("rejected 明细直出：徽标展开回看记录拒因（只读事实面）", async () => {
    const calls: FetchCall[] = [];
    stubFetchPlan(
      {
        ...overviewFor({
          totals: { pending_approval: 0, approved: 0, consumed: 0, rejected: 1, deferred: 0 },
          autonomy: 0,
          pending: [],
          rejected: [
            {
              dedupe_key: "rej-1",
              type: "creator",
              locator: "异环 实机",
              viewer_id: "audience",
              reject_chips: ["太泛", "做不了"],
              reject_note: "主播不玩这品类",
            },
          ],
        }),
      },
      calls,
    );
    renderLeads(calls);
    const badge = await screen.findByTestId("leads-rejected");
    expect(badge.textContent).toContain("已拒绝 1");
    // 记录拒因入 DOM（展开与否都在场——只读事实面可回看）。
    expect(badge.textContent).toContain("太泛");
    expect(badge.textContent).toContain("做不了");
    expect(badge.textContent).toContain("主播不玩这品类");
  });
});
