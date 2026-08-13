import { useEffect, useState } from "react";

import { ADMIN_TOKEN_REQUIRED_EVENT, setAdminToken } from "../api";

/**
 * 写面口令弹窗宿主（2026-08-13 官规「线上触发 AI 更新须过密码」）：
 * api 层 401 → ADMIN_TOKEN_REQUIRED_EVENT 唤醒；输入落 localStorage 仓后
 * 由用户自己重点原按钮（写操作不做透明重放——触发两次全量 run 的鬼影
 * 比多按一次按钮贵得多）。取消失态不吞：原动作的 ApiError 文案已在页面面呈现。
 */
export function AdminLockHost() {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");

  useEffect(() => {
    const onRequired = () => {
      setValue("");
      setOpen(true);
    };
    window.addEventListener(ADMIN_TOKEN_REQUIRED_EVENT, onRequired);
    return () => window.removeEventListener(ADMIN_TOKEN_REQUIRED_EVENT, onRequired);
  }, []);

  if (!open) return null;

  return (
    <div className="admin-lock-veil" role="dialog" aria-modal="true" aria-label="写面口令">
      <form
        className="admin-lock card"
        onSubmit={(event) => {
          event.preventDefault();
          const token = value.trim();
          if (!token) return;
          setAdminToken(token);
          setOpen(false);
        }}
      >
        <h3>写面已上锁</h3>
        <p className="admin-lock-hint">
          触发 / 裁决类写操作需要管理口令。落仓后请<b>重点原按钮</b>重试该操作。
        </p>
        <input
          autoFocus
          type="password"
          placeholder="管理口令（ASCII）"
          value={value}
          onChange={(event) => setValue(event.target.value)}
        />
        <div className="admin-lock-actions">
          <button type="button" className="ghost" onClick={() => setOpen(false)}>
            取消
          </button>
          <button type="submit" disabled={value.trim().length === 0}>
            解锁
          </button>
        </div>
      </form>
    </div>
  );
}
