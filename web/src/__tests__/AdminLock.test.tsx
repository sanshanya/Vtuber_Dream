/**
 * AdminLockHost 口令框钉（官规 2026-08-13「线上触发 AI 更新须过密码」）：
 * - 静默态不渲染；401 事件 → 弹窗出现；
 * - 输入口令提交 → 落 localStorage 仓 + 弹窗自闭（用户重点原按钮——不重放）；
 * - 空口令提交按钮禁用（空白不得入仓）。
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ADMIN_TOKEN_REQUIRED_EVENT } from "../api";
import { AdminLockHost } from "../components/AdminLockHost";

afterEach(() => {
  cleanup();
  localStorage.clear();
});

describe("AdminLockHost", () => {
  it("静默态不渲染；401 事件 → 口令框现身", () => {
    render(<AdminLockHost />);
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent(window, new CustomEvent(ADMIN_TOKEN_REQUIRED_EVENT));
    expect(screen.getByRole("dialog", { name: "写面口令" })).toBeTruthy();
  });

  it("提交口令 → 落仓 + 自闭（不做写操作透明重放）", () => {
    render(<AdminLockHost />);
    fireEvent(window, new CustomEvent(ADMIN_TOKEN_REQUIRED_EVENT));
    const input = screen.getByPlaceholderText("管理口令（ASCII）");
    fireEvent.change(input, { target: { value: "swordfish" } });
    fireEvent.click(screen.getByRole("button", { name: "解锁" }));
    expect(localStorage.getItem("vtuber.admin_token")).toBe("swordfish");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("空白口令 → 解锁钮禁用（空白不得入仓）", () => {
    render(<AdminLockHost />);
    fireEvent(window, new CustomEvent(ADMIN_TOKEN_REQUIRED_EVENT));
    const button = screen.getByRole("button", { name: "解锁" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.change(screen.getByPlaceholderText("管理口令（ASCII）"), {
      target: { value: "   " },
    });
    expect(button.disabled).toBe(true);
  });
});
