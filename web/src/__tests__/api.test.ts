/**
 * api 错误信道钉（ag5-F4 / ag5-F6 / ag4-F3 裁定）：
 * - 非 JSON 错误体不得抛裸 SyntaxError → ApiError(status + 「非 JSON」文案)；
 * - 服务端 {error} 体 → message 原样透传、status 保留（404 供 Streamer 首页空态判别）；
 * - 空体成功 → null 载荷不炸。
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError, api, isApiError } from "../api";

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
