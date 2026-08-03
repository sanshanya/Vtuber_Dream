import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MentionText } from "../components/MentionText";

describe("MentionText 高亮定位", () => {
  it("码点区间切片：CJK 文本上的精确 substring", () => {
    // "看《原神》直播……" — 《=cp1, 原=cp2, 神=cp3, 》=cp4 → interval [1,5)
    const text = "看《原神》直播每场都聊城市设计";
    render(
      <MentionText text={text} spans={[{ mention_id: "m1", text: "《原神》", start_offset: 1, end_offset: 5 }]} />,
    );
    const mark = screen.getByText("《原神》");
    expect(mark.tagName).toBe("MARK");
    expect(mark.textContent).toBe("《原神》");
    // 全串内容顺序守恒（高亮只切片、不改字）。
    expect(mark.parentElement?.textContent).toBe(text);
  });

  it("emoji 扩展平面：按 UTF-16 切片会错标，此钉锁码点语义 vs validate_span", () => {
    // cp: a=0, b=1, 😄=2, c=3, d=4 —— UTF-16 度 😄 占 2 个码元；string.slice(2,3) 会切到半个代理对。
    const text = "ab😄cd";
    const { container } = render(
      <MentionText text={text} spans={[{ mention_id: "m2", text: "😄", start_offset: 2, end_offset: 3 }]} />,
    );
    const mark = container.querySelector("mark");
    expect(mark?.textContent).toBe("😄");
    expect(container.textContent).toBe("ab😄cd");
  });

  it("多 mention 排序 + 重叠让位 + 越界丢弃：展示期数据演化只降级不重报", () => {
    const text = "原神与崩坏星穹铁道";
    const { container } = render(
      <MentionText
        text={text}
        spans={[
          // 越界：直接丢弃
          { mention_id: "bad1", text: "？", start_offset: 90, end_offset: 99 },
          // 倒置：直接丢弃
          { mention_id: "bad2", text: "与", start_offset: 6, end_offset: 2 },
          // 与 "原神" 重叠：后到的让位
          { mention_id: "m3", text: "原神与", start_offset: 0, end_offset: 3 },
          { mention_id: "m1", text: "原神", start_offset: 0, end_offset: 2 },
          { mention_id: "m2", text: "星穹铁道", start_offset: 5, end_offset: 9 },
        ]}
      />,
    );
    const marks = Array.from(container.querySelectorAll("mark")).map((el) => el.textContent);
    expect(marks).toEqual(["原神与", "星穹铁道"]);
    expect(container.textContent).toBe(text);
  });

  it("零 span：渲染原文、无 mark", () => {
    const { container } = render(<MentionText text="平平无奇的弹幕" spans={[]} />);
    expect(container.querySelector("mark")).toBeNull();
    expect(container.textContent).toBe("平平无奇的弹幕");
  });
});
