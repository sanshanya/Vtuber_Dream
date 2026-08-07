/**
 * useStartRun 归一钉团（四个提交口的控制流核心，直取分支）：
 * - 成功：track 登记 run_id 并返回；error/followedId 双清。
 * - 409 错文带在飞 run（ID）：改跟随——返回在飞 id、followedId 置位、error 不置。
 *   409 但错文无 run（ID）字样：跟随失败，按普通错误就地置 error（跟随契约严格）。
 * - 其他错误（422/5xx）：返回 null 且 error=errText 口径。
 * - 重试自愈：先被拒置 error，再提交成功 → error 清零；clearError 手动清零。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RunTrackerProvider, useRunTracker } from "../components/RunTracker";
import { useStartRun } from "../hooks/useStartRun";

function stubPlan(plan: Array<{ status: number; body: string }>) {
  const queue = [...plan];
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => {
      const next = queue.shift() ?? { status: 500, body: JSON.stringify({ error: "存根耗尽" }) };
      return {
        ok: next.status >= 200 && next.status < 300,
        status: next.status,
        text: async () => next.body,
      } as Response;
    }),
  );
}

function wrapper({ children }: { children: React.ReactNode }) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  function Probe() {
    const tracker = useRunTracker();
    return <span data-testid="run-id">{tracker.runId ?? "none"}</span>;
  }
  return (
    <QueryClientProvider client={queryClient}>
      <RunTrackerProvider>
        <Probe />
        {children}
      </RunTrackerProvider>
    </QueryClientProvider>
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useStartRun 提交控制流", () => {
  it("成功：track 登记并返回 run_id", async () => {
    stubPlan([{ status: 200, body: JSON.stringify({ run_id: "run-new" }) }]);
    const { result } = renderHook(() => useStartRun(), { wrapper });
    let id: string | null = null;
    await act(async () => {
      id = await result.current.start({ kind: "full" });
    });
    expect(id).toBe("run-new");
    expect(result.current.error).toBeNull();
    expect(result.current.followedId).toBeNull();
    expect(screen.getByTestId("run-id").textContent).toBe("run-new");
  });

  it("409 错文带 run（ID）：改跟随在飞 id，error 不置", async () => {
    stubPlan([
      {
        status: 409,
        body: JSON.stringify({ error: "已有运行中的 run（run-live），请等待完成" }),
      },
    ]);
    const { result } = renderHook(() => useStartRun(), { wrapper });
    let id: string | null = null;
    await act(async () => {
      id = await result.current.start({ kind: "viewer" });
    });
    expect(id).toBe("run-live");
    expect(result.current.followedId).toBe("run-live");
    expect(result.current.error).toBeNull();
    expect(screen.getByTestId("run-id").textContent).toBe("run-live");
  });

  it("409 但错文无 run（ID）字样：跟随契约未命中，按普通错误就地置 error", async () => {
    stubPlan([{ status: 409, body: JSON.stringify({ error: "互斥冲突" }) }]);
    const { result } = renderHook(() => useStartRun(), { wrapper });
    await act(async () => {
      expect(await result.current.start({ kind: "full" })).toBeNull();
    });
    expect(result.current.error).toBe("互斥冲突");
    expect(result.current.followedId).toBeNull();
    expect(screen.getByTestId("run-id").textContent).toBe("none");
  });

  it("422：返回 null 且 error=errText；重试成功自动清零（先跟随后首发同理）", async () => {
    stubPlan([
      { status: 422, body: JSON.stringify({ error: "kind 取值非法" }) },
      { status: 200, body: JSON.stringify({ run_id: "run-retry" }) },
    ]);
    const { result } = renderHook(() => useStartRun(), { wrapper });
    await act(async () => {
      expect(await result.current.start({ kind: "full" })).toBeNull();
    });
    expect(result.current.error).toBe("kind 取值非法");
    await act(async () => {
      expect(await result.current.start({ kind: "full" })).toBe("run-retry");
    });
    expect(result.current.error).toBeNull();
    expect(screen.getByTestId("run-id").textContent).toBe("run-retry");
  });

  it("clearError 手动清零（不动 followedId）", async () => {
    stubPlan([{ status: 422, body: JSON.stringify({ error: "参数面拒绝" }) }]);
    const { result } = renderHook(() => useStartRun(), { wrapper });
    await act(async () => {
      await result.current.start({ kind: "full" });
    });
    expect(result.current.error).not.toBeNull();
    act(() => result.current.clearError());
    expect(result.current.error).toBeNull();
  });
});
