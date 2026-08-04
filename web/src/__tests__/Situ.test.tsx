/**
 * Situ 钉（ag1-F1 / ag4-F6 / synthetic 标记裁定兑现面）：
 * - 计数徽标四层类名=COUNT_BADGE_LAYERS 表{ai,state,action,action}；
 * - 逐项标注与计数同层（interests→badge ai、situations→badge state、opportunities→badge action）；
 * - communities 渲染出观众计数与描述；
 * - synthetic=true → 合成徽标显形；
 * - 形状漂移（键值为对象）不炸（String 护栏，ag4-F6）。
 */
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { COUNT_BADGE_LAYERS, Situ } from "../components/Situ";

const ANALYSIS = {
  executive_summary: "两名观众围绕《异环》收敛。",
  interest_graph: [
    { entity: "异环", status: "近期上升", evidence_summary: "两名观众独立收藏" },
  ],
  communities: [
    { name: "异环城市与演出讨论群", description: "共同作品一致，角度不同", viewer_ids: ["d1", "d2"] },
  ],
  situations: [{ title: "共同讨论入口", status: "新出现", description: "两个角度" }],
  content_opportunities: [{ title: "观看+投票", format: "素材观看", why_now: "信号汇聚" }],
  content_calendar: [{ day: "周五", slot: "《异环》素材" }],
};

afterEach(cleanup);

describe("Situ 四层 badge（ag1-F1：计数与逐项同层）", () => {
  it("计数徽标类名序 = [ai, state, action, action]", () => {
    render(<Situ analysis={ANALYSIS} />);
    const container = screen.getByTestId("situ-count-badges");
    const classes = Array.from(container.querySelectorAll("span")).map((el) => el.className);
    expect(classes).toEqual(COUNT_BADGE_LAYERS.map((layer) => layer.badgeClass));
    expect(classes).toEqual(["badge ai", "badge state", "badge action", "badge action"]);
    expect(container.textContent).toContain("兴趣实体 1");
    expect(container.textContent).toContain("排期 1");
  });

  it("communities 渲染（观众计数 + 描述）", () => {
    render(<Situ analysis={ANALYSIS} />);
    const block = screen.getByTestId("situ-communities");
    expect(block.textContent).toContain("异环城市与演出讨论群");
    expect(block.textContent).toContain("观众 2");
  });

  it("synthetic=true → synthetic_demo 徽标显形；默认不显", () => {
    const { unmount } = render(<Situ analysis={ANALYSIS} />);
    expect(screen.queryByTestId("situ-synthetic")).toBeNull();
    unmount();
    render(<Situ analysis={ANALYSIS} synthetic />);
    expect(screen.getByTestId("situ-synthetic").textContent).toContain("synthetic_demo");
  });

  it("ag4-F6：键值漂移成对象不炸，落在护栏默认位", () => {
    const drifting = {
      executive_summary: { not: "a string" },
      interest_graph: [{ entity: { weird: true }, status: 5 }],
      situations: [{ title: [1], status: {}, description: null }],
    };
    render(<Situ analysis={drifting} />);
    // executive_summary 非字符串 → 段落不渲染；实体/状态落 "?"/空。
    expect(screen.queryByText("[object Object]")).toBeNull();
    const badges = screen.getByTestId("situ-count-badges");
    expect(badges.textContent).toContain("兴趣实体 1");
  });
});
