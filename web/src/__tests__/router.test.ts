/**
 * 红钉：hash 段解码容错——用户手输 URL 带畸形百分号序列
 * （`%zz`、孤儿 `%`）时 decodeURIComponent 抛 URIError，render 期整页白屏。
 * 修后：畸形段回落原段（路由照常匹配字面段，"* " 收参得原串），不抛。
 */
import { describe, expect, it } from "vitest";

import { decodeSegment, matchRoute } from "../router";

describe("decodeSegment 容错", () => {
  it("正常段照常解码", () => {
    expect(decodeSegment("viewers")).toBe("viewers");
    expect(decodeSegment("%E5%BC%A0%E4%B8%89")).toBe("张三");
  });

  it("畸形百分号序列回落原段、不抛", () => {
    expect(decodeSegment("%zz")).toBe("%zz");
    expect(decodeSegment("100%")).toBe("100%");
    expect(decodeSegment("%E5%BC")).toBe("%E5%BC"); // 截断的 UTF-8 序列（少末字节）
  });
});

describe("matchRoute 既有行为保护", () => {
  it("字面段精确比 + * 收参", () => {
    expect(matchRoute(["viewers", "8877", "tree"])).toEqual({
      page: "viewer-tree",
      params: ["8877"],
    });
    expect(matchRoute(["nope"])).toBeNull();
    expect(matchRoute([])).toEqual({ page: "streamer", params: [] });
  });

  it("畸形段能作为 * 参数原样流过", () => {
    expect(matchRoute(["viewers", "100%", "graph"])).toEqual({
      page: "viewer-graph",
      params: ["100%"],
    });
  });
});
