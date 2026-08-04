import { CNY_TINY_THRESHOLD, TOKEN_RATES_CNY_PER_MILLION } from "./constants";

/** state.json.usage 的原始五键（aggregate_runtime_usage 形状；服务端只出原始值）。 */
export interface UsageRow {
  llm_requests?: number;
  tool_calls?: number;
  input_tokens?: number;
  output_tokens?: number;
  total_tokens?: number;
}

/**
 * 金额估算（元）：按未命中价的两段式求和；无 usage → null（不硬编 0）。
 * ag4-F5：usage 存在但 input/output 任一缺键 → 同样 null——同屏 fmtInt 对缺键
 * 显「—」，金额不得静默 0 化（D8 单屏单口径）。
 */
export function estimateCostCny(usage: UsageRow | null | undefined): number | null {
  if (!usage) return null;
  if (typeof usage.input_tokens !== "number" || typeof usage.output_tokens !== "number") {
    return null;
  }
  const { input_tokens: input, output_tokens: output } = usage;
  if (!Number.isFinite(input) || !Number.isFinite(output)) return null;
  return (
    (input * TOKEN_RATES_CNY_PER_MILLION.input + output * TOKEN_RATES_CNY_PER_MILLION.output) /
    1_000_000
  );
}

/** ¥ 金额格式化：>= 阈值 2 位小数，低于按 4 位（tiny 额度可辨）。 */
export function fmtCny(value: number): string {
  return `¥${value >= CNY_TINY_THRESHOLD ? value.toFixed(2) : value.toFixed(4)}`;
}

/** ISO 时间串 → 本地 `YYYY-MM-DD HH:mm:ss`；非法/空 → 原文（渲染容错，不吞错）。 */
export function fmtTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}

/** 整数千分位（token 数公示用）。 */
export function fmtInt(value: number | null | undefined): string {
  return typeof value === "number" && Number.isFinite(value) ? value.toLocaleString("zh-CN") : "—";
}
