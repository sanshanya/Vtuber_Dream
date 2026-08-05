import { useEffect, useState } from "react";

/**
 * 极简 hash 路由（现布局单房间，room uid 不进路径，页面经 /api/rooms 自解析）。
 * 不引 react-router（M5 依赖面有意收敛）。
 * R2#9 收口：路径表 = 下方 ROUTES 数据表（单一真源，表即文档——原头注双源已删）；
 * App.tsx 只负责页面键 → 组件实体的装配（图谱懒加载缝开在那侧）。
 */

/** 页面键：App.tsx 装配表的主键。 */
export type PageId =
  | "streamer"
  | "viewers"
  | "viewer-tree"
  | "viewer-graph"
  | "live"
  | "leads"
  | "graph"
  | "settings";

export interface RouteEntry {
  /** 段形态：字面段精确比；"*" 吃一段、按序收入 params。 */
  seg: string[];
  page: PageId;
}

/** Z3 定稿序对齐导航：主播介绍 → 舰长列表（+个人树/局部图）→ 直播数据 → 线索账本 → 图谱 → 设置。 */
export const ROUTES: RouteEntry[] = [
  { seg: [], page: "streamer" },
  { seg: ["viewers"], page: "viewers" },
  { seg: ["viewers", "*", "tree"], page: "viewer-tree" },
  { seg: ["viewers", "*", "graph"], page: "viewer-graph" },
  { seg: ["live"], page: "live" },
  { seg: ["leads"], page: "leads" },
  { seg: ["graph"], page: "graph" },
  { seg: ["settings"], page: "settings" },
];

export interface MatchedRoute {
  page: PageId;
  /** "*" 段按序收参（现仅 viewer-tree / viewer-graph 各吃一个 vid）。 */
  params: string[];
}

/** ROUTES 序首中即返；无中 → null（App 负责未知路径空态 + 回链引导）。 */
export function matchRoute(segments: string[]): MatchedRoute | null {
  for (const route of ROUTES) {
    if (route.seg.length !== segments.length) continue;
    const params: string[] = [];
    let hit = true;
    for (let i = 0; i < route.seg.length && hit; i += 1) {
      const want = route.seg[i];
      if (want === "*") params.push(segments[i]);
      else if (want !== segments[i]) hit = false;
    }
    if (hit) return { page: route.page, params };
  }
  return null;
}

/** 轮2-R1-B2：段解码容错——畸形百分号序列（%zz/孤儿 %/截断 UTF-8）让
 *  decodeURIComponent 抛 URIError，render 期整页白屏；回落原段（路由面照走）。 */
export function decodeSegment(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

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
    .map(decodeSegment);
}
// 注：ag4-F10——navigate() 助函数曾导出而零调用，已删；链接纪律 = href="#/..." + 手写编码。
