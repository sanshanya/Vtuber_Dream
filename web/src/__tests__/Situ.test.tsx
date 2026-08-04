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

  it("W3/r1-F5：communities 是 AI 群体推断产物——计数徽标类名钉 badge ai", () => {
    render(<Situ analysis={ANALYSIS} />);
    const block = screen.getByTestId("situ-communities");
    const countBadge = block.querySelector("span.badge");
    expect(countBadge?.className).toBe("badge ai");
  });

  it("synthetic=true → synthetic_demo 徽标显形；默认不显", () => {
    const { unmount } = render(<Situ analysis={ANALYSIS} />);
    expect(screen.queryByTestId("situ-synthetic")).toBeNull();
    unmount();
    render(<Situ analysis={ANALYSIS} synthetic />);
    expect(screen.getByTestId("situ-synthetic").textContent).toContain("synthetic_demo");
  });

  it("W3/r1-F1：合成标记是反事实元信息——裸 badge 不带四层类名", () => {
    render(<Situ analysis={ANALYSIS} synthetic />);
    const badge = screen.getByTestId("situ-synthetic");
    expect(badge.className).toBe("badge");
    for (const layer of ["fact", "ai", "state", "action"]) {
      expect(badge.className).not.toContain(layer);
    }
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

  it("Z2b：content_calendar 不再只计数不呈现（theme/session/goal/验证信号）", () => {
    const cal = {
      ...ANALYSIS,
      content_calendar: [
        {
          session: "近期（1-2周内）",
          theme: "异环1.2版本探索直播",
          goal: "激活舰长互动",
          validation_signal: "舰长弹幕/评论互动量",
        },
      ],
    };
    render(<Situ analysis={cal} />);
    const block = screen.getByTestId("situ-calendar");
    expect(block.textContent).toContain("异环1.2版本探索直播");
    expect(block.textContent).toContain("近期（1-2周内）");
    expect(block.textContent).toContain("激活舰长互动");
    expect(block.textContent).toContain("验证信号：舰长弹幕/评论互动量");
  });

  it("Z2b：摘要以「## 整体态势」开场 → 首标题行剥除（卡名不叠罗汉）", () => {
    const dup = {
      ...ANALYSIS,
      executive_summary: "## 整体态势\n## 观众结构\n- 一\n",
    };
    render(<Situ analysis={dup} />);
    const heads = document.querySelectorAll(".markdown h3");
    expect(heads.length).toBe(1);
    expect(heads[0].textContent).toBe("观众结构");
  });

  it("Z2b：拆卡——各部件独立 section（态势摘要 / 兴趣实体 / 观众社群 / 关键态势 / 行动建议与排期）", () => {
    const cal = { ...ANALYSIS, content_calendar: [{ theme: "t" }] };
    const { container } = render(<Situ analysis={cal} />);
    const titles = Array.from(container.querySelectorAll(".situ-part h2")).map(
      (el) => el.textContent,
    );
    expect(titles).toEqual(["态势摘要", "兴趣实体", "观众社群", "关键态势", "行动建议与排期"]);
  });
});
