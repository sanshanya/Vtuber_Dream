import { useEffect, useState } from "react";

/**
 * 极简 hash 路由：五页面 + 两级参数（现布局单房间，room uid 不再进路径，
 * 页面经 /api/rooms 自解析）。不引 react-router（M5 依赖面有意收敛）。
 *
 * 路径表：
 *   #/                        房间面板
 *   #/viewers                 观众列表（空池单查引导位）
 *   #/viewers/{vid}/tree      个人树
 *   #/viewers/{vid}/graph     局部图
 *   #/graph                   整体图
 *   #/settings                设置
 */
export function useHashPath(): string[] {
  const [hash, setHash] = useState(() => window.location.hash);
  useEffect(() => {
    const onChange = () => setHash(window.location.hash);
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return hash
    .replace(/^#/, "")
    .split("/")
    .filter(Boolean)
    .map((segment) => decodeURIComponent(segment));
}
// 注：ag4-F10——navigate() 助函数曾导出而零调用，已删；链接纪律 = href="#/..." + 手写编码。
