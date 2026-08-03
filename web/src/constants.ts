/**
 * web 侧魔数唯一真源（AGENTS §4：任何阈值必须有命名、默认值、用途和测试）。
 * 与后端对齐的数值在本文件就地写后端出处；金额/时间渲染一律在前端（D8），
 * 服务端只出原始值（token 数 / ISO 时间串）。
 */

/** run 轮询间隔（design §10：轮询 1–2s，取中区；只在 run active 时启用）。 */
export const RUN_POLL_INTERVAL_MS = 1500;

/** events 环渲染上限（= live-core events.rs RUN_EVENTS_CAP）。 */
export const RUN_EVENTS_CAP = 50;

/**
 * 费率（元 / 百万 token）——D8 唯一真源：服务端 state.json.usage 只出原始
 * token 数，金额换算只发生在这里（不进后端逻辑）。
 * DeepSeek-chat 公开价目（2026-08 校准）：input 缓存未命中 ¥2；output ¥8。
 * 前端不具备缓存命中细分 → 公示按未命中价，文案注明「上限估算」。
 */
export const TOKEN_RATES_CNY_PER_MILLION = { input: 2, output: 8 } as const;

/** 金额展示的小数分界：低于此数额按 4 位小数显示，否则 2 位（读感阈值）。 */
export const CNY_TINY_THRESHOLD = 0.01;
