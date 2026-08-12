/**
 * 个人树面（/tree）的时效位钉团——与 Viewers.test.tsx 同形三钉：
 * - ai_stale=true → 感知/AI 区块亮「信源已更新·待重判」徽标（ai-stale-badge-tree）；
 * - ai_stale=false / null → 不亮（绿灯与无参考旧结论两面无差别安静）。
 * 断言一律 getByTestId 锚点（教训：文案与 note 可能同语撞车，不用文本全文匹配）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RunTrackerProvider } from "../components/RunTracker";
import { ViewerTree } from "../pages/ViewerTree";

function stubFetch(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, text: async () => JSON.stringify(body) }) as Response),
  );
}

const BASE_TREE = {
  uid: "demo-1",
  viewer: {
    schema_version: 1,
    viewer: { name: "演示观众A" },
    profile: { face: "" },
    collected_at: "2026-08-03T07:52:20+00:00",
  },
  ai: { status: "complete" },
  ai_stale: null,
  episodes: [],
  mentions: [],
};

function renderTree(over: Record<string, unknown>) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  stubFetch({ ...BASE_TREE, ...over });
  render(
    <QueryClientProvider client={queryClient}>
<RunTrackerProvider>
        <ViewerTree roomId="983" vid="demo-1" />
      </RunTrackerProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("ViewerTree 时效位徽标（Z5c）", () => {
  it("ai_stale=true → 感知区块亮「信源已更新·待重判」，且 Perception 徽标本体保留", async () => {
    renderTree({ ai_stale: true });
    await waitFor(() => expect(screen.getByTestId("ai-stale-badge-tree")).toBeTruthy());
    const badge = screen.getByTestId("ai-stale-badge-tree");
    expect(badge.textContent).toBe("信源已更新·待重判");
    expect(badge.getAttribute("title")).toMatch(/重跑「舰长 AI 分析」后熄灭/);
    expect(screen.getByText("Perception complete")).toBeTruthy();
  });

  it("ai_stale=false → 不亮（时效位绿灯安静）", async () => {
    renderTree({ ai_stale: false });
    await waitFor(() => expect(screen.getByText("Perception complete")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge-tree")).toBeNull();
  });

  it("ai_stale=null → 不亮（无参考旧结论的安静面）", async () => {
    renderTree({ ai_stale: null, ai: { status: null } });
    await waitFor(() => expect(screen.getByText("Perception 未运行")).toBeTruthy());
    expect(screen.queryByTestId("ai-stale-badge-tree")).toBeNull();
  });
});

describe("ViewerTree Episode 时间戳语义徽标（R2 批6 D7）", () => {
  it("双行混排：published 行落「发布于」、缺 published 行落「采集于」，tooltip 文案各自钉", async () => {
    renderTree({
      episodes: [
        {
          episode_id: "e-pub",
          source: "bangumi",
          event_type: "ep",
          published_at: "2026-08-03T03:06:16+00:00",
          observed_at: "2026-08-03T10:00:00+00:00",
          title: "平台给了行为时刻的行",
        },
        {
          episode_id: "e-only-obs",
          source: "bangumi",
          event_type: "ep",
          observed_at: "2026-08-03T03:06:16+00:00",
          title: "平台没给只留采集时刻的行",
        },
      ],
      mentions: [],
    });
    await waitFor(() => expect(screen.getByTestId("episode-ts-e-pub")).toBeTruthy());
    const published = screen.getByTestId("episode-ts-e-pub");
    const collected = screen.getByTestId("episode-ts-e-only-obs");
    // 徽标各自落位（恒落其一：两徽标不互串）。
    expect(published.textContent).toContain("发布于");
    expect(published.textContent).not.toContain("采集于");
    expect(collected.textContent).toContain("采集于");
    expect(collected.textContent).not.toContain("发布于");
    // tooltip 文案钉（时间戳徽标验收原文）。
    expect(published.getAttribute("title")).toBe("发布于=平台显示的行为时刻");
    expect(collected.getAttribute("title")).toBe("采集于=我们看到这条的时刻（非行为时刻）");
  });

  it("published_at 为空串 → 视为平台没给，回落「采集于」", async () => {
    renderTree({
      episodes: [
        {
          episode_id: "e-empty",
          source: "bangumi",
          event_type: "ep",
          published_at: "",
          observed_at: "2026-08-03T03:06:16+00:00",
        },
      ],
      mentions: [],
    });
    await waitFor(() => expect(screen.getByTestId("episode-ts-e-empty")).toBeTruthy());
    const badge = screen.getByTestId("episode-ts-e-empty");
    expect(badge.textContent).toContain("采集于");
    expect(badge.getAttribute("title")).toBe("采集于=我们看到这条的时刻（非行为时刻）");
  });
});
describe("ViewerTree AI 感知结构化块（R3 二次加工）", () => {
  const ANALYSIS = {
    profile_summary: "画像正文一段。整体画像：收口一段。",
    content_preferences: ["太吾绘卷天幕心帷攻略", "NCCL/Kubernetes 系统学习"],
    recent_changes: ["近期收藏重心转向效率攻略"],
    hypotheses: ["假说：或从事 GPU 基础设施相关工作，待佐证"],
    conversation_openers: [
      { title: "聊 DeepMind 四小时访谈", detail: "可自然切入人才流动话题。", evidence_mention_ids: ["m136", "m201"] },
      { title: "无证据开场所", detail: "", evidence_mention_ids: [] },
    ],
    content_ideas: [
      { title: "硬核攻略内容线", detail: "围绕真必死难度展开。", evidence_mention_ids: ["m310"] },
    ],
    cautions: ["以上均为行为信号推断，非内心确证"],
    enrichment_targets: ["天幕心帷最新更新"],
  };

  it("九个结构字段全部落座：徽章/chip/开场卡/点子卡/假说/注意/待补证各自显形", async () => {
    renderTree({ ai: { status: "complete", analysis: ANALYSIS } });
    // 散文折段：有「整体画像：」收口锚 → 两段 <p>。
    await waitFor(() => expect(screen.getByTestId("profile-summary")).toBeTruthy());
    expect(screen.getByTestId("profile-summary").querySelectorAll("p").length).toBe(2);
    // 全局 AI 徽标（紫虚线一档：AI 推断非事实面）。
    expect(screen.getByText("AI 推断 · 非事实面")).toBeTruthy();
    // chips 与清单计数钉。
    expect(screen.getByTestId("block-prefs").querySelectorAll(".chip").length).toBe(2);
    expect(screen.getByTestId("block-changes").querySelectorAll("li").length).toBe(1);
    expect(screen.getByTestId("block-hypotheses").querySelectorAll("li").length).toBe(1);
    expect(screen.getByTestId("block-cautions").querySelectorAll("li").length).toBe(1);
    expect(screen.getByTestId("block-enrich").querySelectorAll(".chip").length).toBe(1);
    // 开场卡：两条；第一条带证据×2 事实徽标，第二条无证据不亮徽标。
    expect(screen.getByTestId("opener-0").textContent).toContain("聊 DeepMind 四小时访谈");
    expect(screen.getByTestId("opener-0").textContent).toContain("证据×2");
    expect(screen.getByTestId("opener-1").textContent).not.toContain("证据×");
    // 点子卡一条带证据×1。
    expect(screen.getByTestId("idea-0").textContent).toContain("硬核攻略内容线");
    expect(screen.getByTestId("idea-0").textContent).toContain("证据×1");
  });

  it("散文无收口锚 → 单段原样（不做脆切），空侧字段整块隐身", async () => {
    renderTree({
      ai: {
        status: "complete",
        analysis: { profile_summary: "就一整段没有收口句的画像。" },
      },
    });
    await waitFor(() => expect(screen.getByTestId("profile-summary")).toBeTruthy());
    expect(screen.getByTestId("profile-summary").querySelectorAll("p").length).toBe(1);
    for (const block of ["block-prefs", "block-changes", "block-hypotheses", "block-cautions", "block-enrich", "block-openers", "block-ideas"]) {
      expect(screen.queryByTestId(block)).toBeNull();
    }
  });

  it("analysis 全空 → 结构块整体不出场（缺席即无，不臆造分区）", async () => {
    renderTree({ ai: { status: "complete", analysis: {} } });
    await waitFor(() => expect(screen.getByText("Perception complete")).toBeTruthy());
    expect(screen.queryByTestId("ai-perception-card")).toBeNull();
  });
});
