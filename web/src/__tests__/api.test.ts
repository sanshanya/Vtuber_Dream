/**
 * api 错误信道钉（错误信封化裁定）：
 * - 非 JSON 错误体不得抛裸 SyntaxError → ApiError(status + 「非 JSON」文案)；
 * - 服务端 {error} 体 → message 原样透传、status 保留（404 供 Streamer 首页空态判别）；
 * - 空体成功 → null 载荷不炸。
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError, api, budgetBlockOf, isApiError } from "../api";

function stubFetch(status: number, body: string) {
  const fake = {
    ok: status >= 200 && status < 300,
    status,
    text: async () => body,
  } as Response;
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => fake),
  );
}

afterEach(() => vi.unstubAllGlobals());

describe("request 错误信道", () => {
  it("非 JSON 错误体 → ApiError：status 保留 + 文案可辨（不抛裸 SyntaxError）", async () => {
    stubFetch(404, "<html>not found</html>");
    const error: unknown = await api.rooms().catch((thrown: unknown) => thrown);
    expect(isApiError(error)).toBe(true);
    expect((error as ApiError).status).toBe(404);
    expect((error as ApiError).message).toContain("非 JSON");
    expect((error as ApiError).message).toContain("404");
  });

  it("服务端 {error} 体 → message 透传服务端文案、status 保留（空态判别数据位）", async () => {
    stubFetch(422, JSON.stringify({ error: "kind 必须是 full 或 viewer" }));
    const error: unknown = await api.run("any").catch((thrown: unknown) => thrown);
    expect(isApiError(error)).toBe(true);
    expect((error as ApiError).status).toBe(422);
    expect((error as ApiError).message).toBe("kind 必须是 full 或 viewer");
  });

  it("404 + {error} → ApiError(status=404)，Streamer 首页靠它而非文案子串判空态", async () => {
    stubFetch(404, JSON.stringify({ error: "尚无 collection 完成快照…" }));
    const error: unknown = await api.overview("983").catch((thrown: unknown) => thrown);
    expect(isApiError(error)).toBe(true);
    expect((error as ApiError).status).toBe(404);
  });

  it("200 + 合法 JSON → 正常解析", async () => {
    stubFetch(200, JSON.stringify([{ id: "983", project_name: "p", streamer_uid: "1", output_dir: "o" }]));
    const rooms = await api.rooms();
    expect(rooms[0]?.id).toBe("983");
  });

  it("空体成功响应 → null 载荷（不得炸 parse）", async () => {
    stubFetch(200, "");
    expect(await api.rooms()).toBeNull();
  });
});

describe("Z6 件3/件5 预算面客户端与析取", () => {
  it("api.budget() → BudgetInfo 全段解析（budget_cny null 透传 + D8 estimate 段）", async () => {
    stubFetch(
      200,
      JSON.stringify({
        budget_cny: null,
        month: "2026-08",
        month_cost_cny: 0.35,
        month_runs: 3,
        last_run: {
          run_id: "r-1",
          ts: "2026-08-05T00:00:00+00:00",
          cost_cny: 3.0,
          status: "failed",
          kind: "full",
          spend_mode: "normal",
        },
        estimate: {
          roster_viewers: 22,
          normal_cny: 34.5,
          briefing_cny: 1.5,
          etd_minutes: [17, 35],
        },
      }),
    );
    const info = await api.budget();
    expect(info.month).toBe("2026-08");
    expect(info.budget_cny).toBeNull();
    expect(info.month_cost_cny).toBe(0.35);
    expect(info.month_runs).toBe(3);
    expect(info.last_run?.kind).toBe("full");
    expect(info.estimate?.normal_cny).toBe(34.5);
    expect(info.estimate?.etd_minutes).toEqual([17, 35]);
  });

  it("api.budget() → estimate 段缺席/全 null 透传为缺省（前端「预估 —」读面）", async () => {
    stubFetch(
      200,
      JSON.stringify({
        budget_cny: 4.0,
        month: "2026-08",
        month_cost_cny: 0,
        month_runs: 0,
        last_run: null,
        estimate: { roster_viewers: null, normal_cny: null, briefing_cny: null, etd_minutes: null },
      }),
    );
    const info = await api.budget();
    expect(info.estimate?.normal_cny).toBeNull();
    expect(info.estimate?.etd_minutes).toBeNull();
  });

  it("budgetBlockOf：完整 budget_block → 六键析出；缺预算块/数字不齐 → null", () => {
    const block = {
      spend_mode: "normal",
      estimated_cny: 3.0,
      budget_cny: 0.01,
      fresh_viewers: 1,
      total_viewers: 1,
      hint: "两选重发：spend_mode=incremental 只更新变化者 / briefing_only 只推简报",
    };
    expect(budgetBlockOf({ budget_block: block })).toEqual(block);
    // 无 budget_block → null（其他 failed 体不误触卡）。
    expect(budgetBlockOf({ error: "采集 404" })).toBeNull();
    expect(budgetBlockOf(null)).toBeNull();
    // 数字不齐（estimated 非有限）→ null，不宣布阻断。
    expect(
      budgetBlockOf({ budget_block: { ...block, estimated_cny: "3.0" } }),
    ).toBeNull();
  });
});
