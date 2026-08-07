/**
 * P0-2 钉团（迭代细则 v1 §1 验收钉②③④在前端的形态）：
 * - ready 态：四数+命名件+未知行恒在；
 * - naming=null：动作位具名「未命名」，不补文案（无伪造语义）；
 * - empty 态：诚实文案（今晚没人来——但你没说错话）而非报错；
 * - null 态：复盘尚未生成；
 * - Streamer 集成（v2 P1-1/D6）：复盘卡为首卡；宏观折叠段已整段退役。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RecapCard, type RecapPayload } from "../components/RecapCard";
import { RunTrackerProvider } from "../components/RunTracker";
import { Streamer } from "../pages/Streamer";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function renderCard(recap: RecapPayload | null) {
  render(<RecapCard recap={recap} />);
}

const READY: RecapPayload = {
  status: "ready",
  generated_at: "2026-08-05T22:00:00+00:00",
  session: { start: "2026-08-05T21:00:00+00:00", end: "2026-08-05T21:30:00+00:00", rid: "S2" },
  headline: "今晚 3 人来过，1 人回来过（前 1 场见过他们）；最密的十分钟有 3 行弹幕；「晚上好！」被刷了 3 次",
  speakers: 3,
  returning: { count: 1, base: 3, sessions_back: 1 },
  peak: { start: "2026-08-05T21:10:00+00:00", count: 3, window_minutes: 10 },
  repeated: { text: "晚上好！", count: 3 },
  naming: {
    peak_name: "落雨十分钟",
    sentence_name: "晚好三连",
    reuse_line: "明天把「晚上好」留成仪式句，复用",
    cut_advice: "以 21:10±2min 切开场互动切片",
    named_at: "2026-08-05T22:00:00+00:00",
  },
  unknown: [],
  empty_copy: null,
};

describe("下播复盘卡", () => {
  it("ready：一句话结论 + 三证据 + 动作 + 未知行恒在（钉④）", () => {
    renderCard(READY);
    // 一句话结论
    expect(screen.getByTestId("recap-headline").textContent).toContain("3 人来过");
    // 三个证据（四数可复算形态：分子/分母明示）
    const evidence = screen.getByTestId("recap-evidence");
    expect(evidence.textContent).toContain("3");
    expect(evidence.textContent).toContain("1/3 回来过");
    expect(evidence.textContent).toContain("3 行");
    expect(evidence.textContent).toContain("「晚上好！」");
    expect(evidence.textContent).toContain("复读 ×3");
    // AI 命名件落位
    const action = screen.getByTestId("recap-action");
    expect(action.textContent).toContain("复用");
    expect(action.textContent).toContain("切片切口");
    // 未知行恒存在（零内容时也要成行：暂无）
    const unknown = screen.getByTestId("recap-unknown");
    expect(unknown.textContent).toContain("未知的部分");
    expect(unknown.textContent).toContain("暂无");
  });

  it("naming=null：动作位具名未命名、未知行显内容（无伪造语义）", () => {
    renderCard({ ...READY, naming: null, unknown: ["AI 命名未达成：recap-naming failed"] });
    expect(screen.getByTestId("recap-action").textContent).toContain("待 AI 命名");
    expect(screen.getByTestId("recap-unknown").textContent).toContain("AI 命名未达成");
  });

  it("empty：诚实文案而非报错（验收钉②）", () => {
    renderCard({
      status: "empty",
      headline: "今晚没人来——但你没说错话。",
      empty_copy: "今晚没人来——但你没说错话。",
      speakers: 0,
      unknown: ["本场（最新场次窗）零发言。"],
    });
    expect(screen.getByTestId("recap-empty-copy").textContent).toContain(
      "今晚没人来——但你没说错话",
    );
    expect(screen.getByTestId("recap-unknown").textContent).toContain("零发言");
  });

  it("null：复盘尚未生成——具名缺席（缺席必可见）", () => {
    renderCard(null);
    expect(screen.getByText(/复盘尚未生成/)).toBeTruthy();
    expect(screen.getByTestId("recap-unknown").textContent).toContain("未知的部分");
  });
});

describe("Streamer 集成（v2 P1-1 首卡钉 + R2 批5 D6 退役面）", () => {
  it("复盘卡上页面且为首卡（DOM 序领先简报卡）；宏观折叠组整段退役零残留", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes("/overview")) {
          return {
            ok: true,
            status: 200,
            text: async () =>
              JSON.stringify({
                collection: { status: "complete", finished_at: "2026-08-05T00:00:00+00:00" },
                ai: { status: "complete" },
                situation: {
                  status: "complete",
                  analysis: { situations: [{ headline: "h", detail: "d" }] },
                },
                leads: { totals: {}, pending: [] },
                recap: READY,
              }),
          } as Response;
        }
        if (url.includes("/viewers")) {
          return { ok: true, status: 200, text: async () => JSON.stringify([]) } as Response;
        }
        return { ok: false, status: 404, text: async () => JSON.stringify({ error: "?" }) } as Response;
      }),
    );
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
    render(
      <QueryClientProvider client={queryClient}>
        <RunTrackerProvider>
          <Streamer roomId="983" />
        </RunTrackerProvider>
      </QueryClientProvider>,
    );
    await waitFor(() => expect(screen.getByTestId("recap-card")).toBeTruthy());
    // 复盘卡呈现四数
    expect(screen.getByTestId("recap-headline").textContent).toContain("3 人来过");
    // v2 P1-1（裁决三）：首卡恒为复盘卡——DOM 序领先简报卡。
    const recapCard = screen.getByTestId("recap-card");
    const briefingCard = screen.getByTestId("briefing-card");
    expect(
      recapCard.compareDocumentPosition(briefingCard) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    // R2 批5 D6：宏观段整段退役——旧「整体态势（宏观）…项，点开看」零残留。
    expect(screen.queryByTestId("macro-details")).toBeNull();
      // 「整体态势」heading 是退役段的签名（动作区 note 里的同名短语是存活文案）。
      expect(screen.queryByRole("heading", { name: "整体态势" })).toBeNull();
      expect(screen.queryByText(/点开看/)).toBeNull();
  });
});
