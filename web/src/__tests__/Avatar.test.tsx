/**
 * Avatar 统一头像面钉团：
 * - face 有 → img no-referrer 防盗链（理由源头注在 components/Avatar.tsx）+ 尺寸档位类；
 * - face 空串/null → 首字 fallback 块 + role="img" + aria-label（可达性面）；
 * - name 缺 → 「?」占位，不臆造字符。
 */
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { Avatar } from "../components/Avatar";

afterEach(cleanup);

describe("Avatar（R2#7）", () => {
  it("face 有 → img + referrerPolicy=no-referrer + loading=lazy + 档位类", () => {
    const { container } = render(
      <Avatar face="https://i0.hdslb.com/bfs/face/a.jpg" name="有头" size="sm" />,
    );
    const img = container.querySelector("img.avatar.avatar-sm");
    expect(img).not.toBeNull();
    expect(img?.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(img?.getAttribute("loading")).toBe("lazy");
    expect(container.querySelector(".avatar-fallback")).toBeNull();
  });

  it("face 空串 → 首字 fallback + role=img + aria-label 直陈谁的占位", () => {
    const { container } = render(<Avatar face="" name="无头" size="xs" />);
    const fallback = container.querySelector(".avatar.avatar-xs.avatar-fallback");
    expect(fallback).not.toBeNull();
    expect(fallback?.textContent).toBe("无");
    expect(fallback?.getAttribute("role")).toBe("img");
    expect(fallback?.getAttribute("aria-label")).toBe("无头 头像");
    expect(container.querySelector("img")).toBeNull();
  });

  it("face null + name 缺 → 「?」占位，aria-label 落「无头像」", () => {
    const { container } = render(<Avatar face={null} name={null} />);
    const fallback = container.querySelector(".avatar.avatar-fallback");
    expect(fallback?.textContent).toBe("?");
    expect(fallback?.getAttribute("aria-label")).toBe("无头像");
  });
});
