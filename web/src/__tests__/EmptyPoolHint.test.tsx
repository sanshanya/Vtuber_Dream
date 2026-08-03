import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { EmptyPoolHint } from "../pages/Viewers";

/** 空池引导位是纯组件：uid 接线 + 提交时序由 props 走，不摸 fetch。 */
function Harness(props: { onSubmit: () => void }) {
  const [uid, setUid] = useState("");
  return <EmptyPoolHint uid={uid} pending={false} onUidChange={setUid} onSubmit={props.onSubmit} />;
}

describe("空池单查引导位（§12 冷启动）", () => {
  it("引导文案 + 输入接线 + 提交回调", () => {
    const onSubmit = vi.fn();
    render(<Harness onSubmit={onSubmit} />);
    // 引导位呈现
    expect(screen.getByTestId("empty-pool-hint")).toBeDefined();
    expect(screen.getByText("观众池为空")).toBeDefined();
    // 空 uid → 钮禁态
    const button = screen.getByRole("button");
    expect(button).toHaveProperty("disabled", true);
    // 键入 uid → 解禁 + 提交冒泡
    fireEvent.change(screen.getByLabelText("单查观众 uid"), { target: { value: " 12345 " } });
    expect(button).toHaveProperty("disabled", false);
    fireEvent.click(button);
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });

  it("pending 态：钮禁 + 文安暗示已提交", () => {
    render(<EmptyPoolHint uid="42" pending={true} onUidChange={() => {}} onSubmit={() => {}} />);
    const button = screen.getByRole("button");
    expect(button).toHaveProperty("disabled", true);
    expect(button.textContent).toBe("已提交…");
  });
});
