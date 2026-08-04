/**
 * /api 客户端薄层（D3 端点表逐行兑现）。
 * run 记录与 config 面按真实响应收窄成接口；viewer/tree/graph 体量大、
 * 渲染面只「前进式取用」 → 保持 loose，转发即渲染。
 */

const API_BASE = "/api";

/**
 * 结构化错误：HTTP status + 用户可读文案（ag5-F4/F6）。
 * Streamer 首页空态等分支用 status 判别，绝不再子串匹配 message。
 */
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError;
}

export interface Room {
  id: string;
  project_name: string;
  streamer_uid: string;
  output_dir: string;
}

export interface RunRecordView {
  run_id: string;
  kind: string;
  viewer_uid: string | null;
  force: boolean;
  /** queued | collecting | episodes | per_viewer_ai | audience | done | failed（design §10）。 */
  status: string;
  started_at: string;
  finished_at: string | null;
  /** 终态且有观众级失败（done 时 viewer_failures>0 或 failed 于 audience 段）。 */
  partial: boolean;
  outcome: unknown;
  events: string[];
}

export interface ViewerRow {
  uid: string;
  name: string | null;
  /** Z3d：大航海身份面（face 为空串 → null 由服务端 as_str 直接漏空，前端须判空）。 */
  face: string | null;
  /** 大航海等级：3=舰长 / 2=提督 / 1=总督。 */
  guard_level: number | null;
  medal_level: number | null;
  collected_at: string | null;
  ai_status: string | null;
  ai_completed: boolean;
  /** Z5c 时效位：true=旧 AI 结论的信源已更新（哈希翻），false=时效内绿灯，null=无参考旧结论。 */
  ai_stale: boolean | null;
}

export interface ConfigView {
  project_name: string;
  output_dir: string;
  bilibili: {
    room_id: string;
    streamer_uid: string;
    cookie_present: boolean;
    additional_viewer_ids: string[];
  };
  ai: {
    api: string;
    base_url: string;
    model: string;
    api_key_present: boolean;
    [key: string]: unknown;
  };
  writable_keys: string[];
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  // ag5-F4：HTML 错误页/半截代理体曾直接崩 JSON.parse 成裸 SyntaxError 糊脸。
  let payload: unknown = null;
  if (text.length > 0) {
    try {
      payload = JSON.parse(text);
    } catch {
      throw new ApiError(response.status, `服务返回非 JSON 响应（HTTP ${response.status}）`);
    }
  }
  if (!response.ok) {
    const detail =
      payload && typeof payload === "object" && "error" in payload
        ? String((payload as { error: unknown }).error)
        : `HTTP ${response.status}`;
    throw new ApiError(response.status, detail);
  }
  return payload as T;
}

export const api = {
  rooms: () => request<Room[]>("GET", "/rooms"),
  overview: (roomId: string) => request<any>("GET", `/rooms/${encodeURIComponent(roomId)}/overview`),
  viewers: (roomId: string) =>
    request<ViewerRow[]>("GET", `/rooms/${encodeURIComponent(roomId)}/viewers`),
  viewerTree: (roomId: string, vid: string) =>
    request<any>(
      "GET",
      `/rooms/${encodeURIComponent(roomId)}/viewers/${encodeURIComponent(vid)}/tree`,
    ),
  viewerGraph: (roomId: string, vid: string) =>
    request<{ elements: unknown[] }>(
      "GET",
      `/rooms/${encodeURIComponent(roomId)}/viewers/${encodeURIComponent(vid)}/graph`,
    ),
  roomGraph: (roomId: string) =>
    request<{ elements: unknown[] }>("GET", `/rooms/${encodeURIComponent(roomId)}/graph`),
  config: () => request<ConfigView>("GET", "/config"),
  putConfig: (body: unknown) => request<{ status: string; keys?: number }>("PUT", "/config", body),
  run: (id: string) => request<RunRecordView>("GET", `/runs/${encodeURIComponent(id)}`),
  startRun: (body: { kind: RunKind; force?: boolean; viewer_uid?: string }) =>
    request<{ run_id: string }>("POST", "/runs", body),
};

/** Z4 动作平面：六 kind 字面冻结（与 registry.rs RUN_KINDS/RUN_KINDS_STAGED 同源）。 */
export const RUN_KIND_LABELS = {
  full: "全量感知",
  viewer: "单舰长感知",
  collect_streamer: "主播采集",
  collect_guards: "舰长采集",
  ai_viewers: "舰长 AI 分析",
  ai_audience: "主播 AI 分析",
} as const;

export type RunKind = keyof typeof RUN_KIND_LABELS;

/** 服务端 409 错文「已有进行中的 run（{id}），…」的在飞 id 抽取（nil → 纯报错面）。 */
export function activeRunIdFrom(message: string): string | null {
  const hit = /run（([^）]+)）/.exec(message);
  return hit ? hit[1] : null;
}
