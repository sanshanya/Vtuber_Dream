import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { DeltaBlock } from "../components/DeltaBlock";

describe("DeltaBlock 基线态 vs 差分态（D4 口径）", () => {
  it("baseline_only=true（首轮/单边）一律「基线已建」，不渲染空表格", () => {
    render(<DeltaBlock delta={{ baseline_only: true, from_run_id: null, to_run_id: null }} />);
    expect(screen.getByTestId("delta-baseline")).toBeDefined();
    expect(screen.getByText("基线已建")).toBeDefined();
    expect(screen.queryByTestId("delta-diff")).toBeNull();
  });

  it("非基线：三列迁移计数 + from/to peak 迁移串 + 舰长增减", () => {
    render(
      <DeltaBlock
        delta={{
          baseline_only: false,
          from_run_id: "run-1",
          to_run_id: "run-2",
          interest: {
            opened: [
              { viewer_id: "100", entity_id: "e1", canonical_name: "异环", status: "active", preference: "like" },
            ],
            closed: [],
            changed: [
              {
                viewer_id: "100",
                entity_id: "e2",
                canonical_name: "Blender",
                from: { status: "active", preference: "like" },
                to: { status: "active", preference: "love" },
              },
            ],
          },
          guards: { added: ["200"], removed: ["300"] },
        }}
      />,
    );
    expect(screen.getByTestId("delta-diff")).toBeDefined();
    // 计数徽章
    expect(screen.getByText("新增 1")).toBeDefined();
    expect(screen.getByText("关闭 0")).toBeDefined();
    expect(screen.getByText("迁移 1")).toBeDefined();
    // 迁移串：preference like→love（status 同值不渲染）；li 文本跨多元素，函数匹配 textContent。
    expect(
      screen.getByText(
        (_, element) =>
          element?.tagName === "LI" &&
          element.textContent === "↻ Blender @100 preference like→love",
      ),
    ).toBeDefined();
    // 舰长区
    expect(screen.getByText("＋1")).toBeDefined();
    expect(screen.getByText("−1")).toBeDefined();
    // 基线徽章不得出现。
    expect(screen.queryByTestId("delta-baseline")).toBeNull();
  });
});
