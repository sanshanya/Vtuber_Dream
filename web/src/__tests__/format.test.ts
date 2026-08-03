import { describe, expect, it } from "vitest";

import { CNY_TINY_THRESHOLD, TOKEN_RATES_CNY_PER_MILLION } from "../constants";
import { estimateCostCny, fmtCny, fmtInt, fmtTime } from "../format";

describe("金额换算唯一真源（D8：服务端只出原始 token 数）", () => {
  it("两段式求和：input/output 按命名费率折算", () => {
    const usage = { input_tokens: 2_000_000, output_tokens: 500_000, total_tokens: 2_500_000 };
    const cost = estimateCostCny(usage);
    expect(cost).toBeCloseTo(
      (2_000_000 * TOKEN_RATES_CNY_PER_MILLION.input +
        500_000 * TOKEN_RATES_CNY_PER_MILLION.output) /
        1_000_000,
      10,
    );
    // 2×2 + 0.5×8 = 8 元
    expect(cost).toBeCloseTo(8, 10);
  });

  it("无 usage → null（不硬编 0；面板不公示假值）", () => {
    expect(estimateCostCny(null)).toBeNull();
    expect(estimateCostCny(undefined)).toBeNull();
  });

  it("fmtCny 阈值切档", () => {
    expect(fmtCny(0.0042)).toBe("¥0.0042");
    expect(fmtCny(CNY_TINY_THRESHOLD)).toBe("¥0.01");
    expect(fmtCny(12.345)).toBe("¥12.35");
  });
});

describe("时间/计数渲染在前端", () => {
  it("fmtTime：ISO 合法 → 本地形；空 → —；非法原文回显（渲染容错不吞错）", () => {
    expect(fmtTime(undefined)).toBe("—");
    expect(fmtTime("")).toBe("—");
    expect(fmtTime("not-a-date")).toBe("not-a-date");
    expect(fmtTime("2026-08-03T10:20:30+00:00")).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/);
  });

  it("fmtInt：千分位 + 空值回退", () => {
    expect(fmtInt(1_234_567)).toBe("1,234,567");
    expect(fmtInt(undefined)).toBe("—");
  });
});
