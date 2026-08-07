import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SituationDetailCard, parseSituationDetail } from "../components/SituationDetailCard";

const FULL = {
  executive_summary: "  本周主线：恐怖游戏联机 + 二游情报双轨。 ",
  content_calendar: [
    { session: "周五夜场", theme: "恐怖联机", target_viewers: ["a", "b"], goal: "整活", validation_signal: "在线峰值" },
  ],
  situations: [
    {
      title: "至冬热潮",
      status: "上升",
      description: "奥黛塔 PV 连锁反应",
      trigger_events: ["PV 发布"],
      evidence_mention_ids: ["m1", "m2"],
      viewer_ids: ["v1"],
    },
  ],
  audience_structure: ["核心 4 人", "创作者 6 人"],
  communities: [
    {
      name: "二游情报圈",
      description: "原神鸣潮双修",
      shared_angles: ["PV 解读", "版本前瞻"],
      viewer_ids: ["v1", "v2"],
      evidence_mention_ids: ["m1"],
      confidence: 0.8,
    },
  ],
  individual_highlights: [
    { viewer_id: "266690713", insight: "收藏恐怖专场", opportunity: "可当联机身位", evidence_mention_ids: ["m9"] },
  ],
  data_gaps: ["缺少弹幕情绪时间轴"],
  safety_notes: ["不点名追问观众"],
};

const NAMES = new Map([["266690713", "15ANGNOH3C"]]);

describe("parseSituationDetail 形状护栏（逐面独立，不连坐）", () => {
  it("全件在场 → 逐面解析成列", () => {
    const d = parseSituationDetail(FULL)!;
    expect(d.executiveSummary).toContain("恐怖游戏联机");
    expect(d.calendar).toHaveLength(1);
    expect(d.situations![0].evidenceCount).toBe(2);
    expect(d.structureLines).toHaveLength(2);
    expect(d.communities![0].sharedAngles).toContain("PV 解读");
    expect(d.highlights![0].viewerId).toBe("266690713");
    expect(d.dataGaps).toEqual(["缺少弹幕情绪时间轴"]);
    expect(d.safetyNotes).toEqual(["不点名追问观众"]);
  });

  it("单面漂移只落该面：calendar 非数组 → 该面 null，其余不伤", () => {
    const d = parseSituationDetail({ ...FULL, content_calendar: "broken" })!;
    expect(d.calendar).toBeNull();
    expect(d.situations).toHaveLength(1);
    expect(d.executiveSummary).not.toBeNull();
  });

  it("项内标题缺席 → 该项跳过（不恒造空行）；字符串白边收净", () => {
    const d = parseSituationDetail({ ...FULL, situations: [{ no_title: 1 }, FULL.situations[0]] })!;
    expect(d.situations).toHaveLength(1);
    expect(d.situations![0].title).toBe("至冬热潮");
  });
});

describe("SituationDetailCard 呈现", () => {
  it("未生成（非 complete）→ 不渲染", () => {
    const { container } = render(
      <SituationDetailCard situationStatus="running" analysis={FULL} nameOf={NAMES} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("就绪：摘要直陈 + 各面计数折叠 + 个人亮点 chip 跳个人树（nameOf 解析）", () => {
    render(<SituationDetailCard situationStatus="complete" analysis={FULL} nameOf={NAMES} />);
    expect(screen.getByTestId("executive-summary").textContent).toContain("恐怖游戏联机");
    expect(screen.getByTestId("face-calendar").textContent).toContain("内容日历 · 1 条");
    expect(screen.getByTestId("face-situations").textContent).toContain("话题场 · 1 条");
    expect(screen.getByTestId("face-communities").textContent).toContain("社群 · 1 条");
    const link = screen.getByText("15ANGNOH3C") as HTMLAnchorElement;
    expect(link.getAttribute("href")).toContain("#/viewers/266690713/tree");
  });

  it("缺席必可见：空面各落「本轮无」行，不整块消失", () => {
    render(
      <SituationDetailCard
        situationStatus="complete"
        analysis={{ content_calendar: [], data_gaps: [], audience_structure: [] }}
        nameOf={NAMES}
      />,
    );
    expect(screen.getByTestId("executive-summary-none")).not.toBeNull();
    expect(screen.getByTestId("face-calendar-none").textContent).toContain("本轮无");
    expect(screen.getByTestId("face-situations-none")).not.toBeNull();
    expect(screen.getByTestId("face-communities-none")).not.toBeNull();
    expect(screen.getByTestId("face-highlights-none")).not.toBeNull();
    expect(screen.getByTestId("face-gaps-none")).not.toBeNull();
    expect(screen.getByTestId("face-safety-none")).not.toBeNull();
  });
});
