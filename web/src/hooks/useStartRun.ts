/**
 * F1 控制流归一：四个 run 提交口（RunButton 双段确认 / KindRunButton /
 * SingleViewerRunButton / Viewers 空池单查）同构的提交控制流收口于此——
 *
 * - 成功：run_id 登记进全局 RunTracker，返回该 id；
 * - 409（单飞互斥契约）：错文里的在飞 run_id 提取后改为跟随（activeRunIdFrom），
 *   返回在飞 id 并置 followedId（调用方据此切换跟随文案，不裸报错）；
 * - 其他错误（422 参数面、5xx）：errText 就地置 error，返回 null（=被拒且 error 已置）。
 *
 * 调用方差异（确认段、pending、文案、后续展示）留在各组件；hook 只归一提交控制流。
 */
import { useCallback, useState } from "react";

import { activeRunIdFrom, api } from "../api";
import { useRunTracker } from "../components/RunTracker";
import { errText } from "../format";

/** api.startRun 请求体（与 /runs 端点契约同源，不另行造词）。 */
export type StartRunBody = Parameters<typeof api.startRun>[0];

export interface UseStartRun {
  /** 提交 run；返回被追踪的 run_id（首发或 409 跟随的在飞 id），被拒 → null 且 error 已置。 */
  start: (body: StartRunBody) => Promise<string | null>;
  /** 最近一次被拒的就地错文（errText 口径）；start/clearError 清。 */
  error: string | null;
  clearError: () => void;
  /** 最近一次 409 跟随的在飞 run_id（无跟随 → null）；start 清。 */
  followedId: string | null;
}

export function useStartRun(): UseStartRun {
  const { track } = useRunTracker();
  const [error, setError] = useState<string | null>(null);
  const [followedId, setFollowedId] = useState<string | null>(null);

  const clearError = useCallback(() => {
    setError(null);
  }, []);

  const start = useCallback(
    async (body: StartRunBody): Promise<string | null> => {
      setError(null);
      setFollowedId(null);
      try {
        const { run_id } = await api.startRun(body);
        track(run_id);
        return run_id;
      } catch (thrown) {
        const text = errText(thrown);
        // 409：服务端互斥——错文携带在飞 run_id，改跟随而非裸报错（后端契约字样）。
        const active = activeRunIdFrom(text);
        if (active) {
          track(active);
          setFollowedId(active);
          return active;
        }
        setError(text);
        return null;
      }
    },
    [track],
  );

  return { start, error, clearError, followedId };
}
