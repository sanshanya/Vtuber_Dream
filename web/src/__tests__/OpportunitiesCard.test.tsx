import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { OpportunitiesCard, parseOpportunities } from "../components/OpportunitiesCard";

const SAMPLE = {
  title: "周末恐怖游戏/联机专场（R.E.P.O + 后室/人窟日记同款）",
  entity: "R.E.P.O",
  why_now: "奶芙恐怖专场被核心观众收藏回放。",
  why_fit: "深夜档与节目效果适配。",
  format: "夜间整活/联机直播场",
  run_of_show: ["开场歌杂", "进入联机"],
  talking_points: ["这波该撤还是该冲"],
  observation_metrics: ["峰值在线"],
  caveats: ["不点名追问互动"],
  evidence_mention_ids: ["mention:1:aaa", "mention:1:bbb"],
  search_result_ids: ["s1"],
  audience_ids: ["266690713", "14213360"],
  confidence: "高",
};

describe("parseOpportunities 形状护栏", () => {
  it("键缺席 / 非数组 / 项漂移 → null 或跳过（按沉默态渲染）", () => {
    expect(parseOpportunities(undefined)).toBeNull();
    expect(parseOpportunities({})).toBeNull();
    expect(parseOpportunities({ content_opportunities: "nope" })).toBeNull();
    // 数组在但项全坏 → 空列表（沉默位同样接住）
    expect(
      parseOpportunities({ content_opportunities: [42, { no_title: true }, null] }),
    ).toEqual([]);
    // 好项存活，缺省面落 null/0（不臆造）
    const [op] = parseOpportunities({ content_opportunities: [SAMPLE, { title: "  " }] })!;
    expect(op).not.toBeUndefined();
    expect(parseOpportunities({ content_opportunities: [SAMPLE, { title: "  " }] })).toHaveLength(1);
    expect(op.title).toContain("恐怖游戏");
    expect(op.entity).toBe("R.E.P.O");
    expect(op.evidenceCount).toBe(2);
    expect(op.audienceCount).toBe(2);
    expect(op.searchCount).toBe(1);
  });
});

describe("OpportunitiesCard 三态", () => {
  it("未生成（非 complete）→ 不渲染（话头归简报卡）", () => {
    const { container } = render(
      <OpportunitiesCard situationStatus="running" analysis={{ content_opportunities: [SAMPLE] }} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("沉默渠：complete 而空数组 → 显式「证据不足」位，不造假卡", () => {
    render(<OpportunitiesCard situationStatus="complete" analysis={{ content_opportunities: [] }} />);
    expect(screen.getByTestId("opportunities-silent-slot").textContent).toContain(
      "本轮证据不足以出内容机会",
    );
    expect(screen.queryByTestId("opportunities-list")).toBeNull();
    // complete 而键缺席（旧轮产出）→ 同沉默位
    render(<OpportunitiesCard situationStatus="complete" analysis={{}} />);
    expect(screen.getAllByTestId("opportunities-silent-slot")).toHaveLength(2);
  });

  it("就绪：标题/置信/实体 + 两段直陈 + 折叠排场与佐证计数", () => {
    render(<OpportunitiesCard situationStatus="complete" analysis={{ content_opportunities: [SAMPLE] }} />);
    expect(screen.getByTestId("opportunities-list")).not.toBeNull();
    expect(screen.getByText(/周末恐怖游戏/)).not.toBeNull();
    expect(screen.getByTestId("opportunity-confidence-0").textContent).toContain("高");
    expect(screen.getByText(/为何是现在/)).not.toBeNull();
    expect(screen.getByText(/为何适合本房/)).not.toBeNull();
    const detail = screen.getByTestId("opportunity-detail-0");
    expect(detail.textContent).toContain("覆盖观众 2");
    expect(detail.textContent).toContain("证据 2 条");
    expect(detail.textContent).toContain("搜索佐证 1 条");
    expect(detail.textContent).toContain("开场歌杂");
    expect(detail.textContent).toContain("不点名追问互动");
  });
});
