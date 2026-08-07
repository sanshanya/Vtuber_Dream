/**
 * /api 客户端薄层（端点表逐行兑现）。
 * run 记录与 config 面按真实响应收窄成接口；overview/tree 两面收口成
 * 分段接口（已知键给字面量，index signature 慎用）——分段与服务端 app.rs 的
 * json! 键一一对应；graph 体量大仍保持 loose，转发即渲染。
 */

import type { LeadsView } from "./components/LeadsBlock";
import type { MentionSpanLike } from "./components/MentionText";
import type { StreamerProfile } from "./components/StreamerCard";
import type { UsageRow } from "./format";

const API_BASE = "/api";

/**
 * 结构化错误：HTTP status + 用户可读文案。
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

/** 终局 outcome.budget_block 阻断面——服务端固定六键；前端只做
 *  presence 判别读取，数字不齐/缺键不臆造（renderer 见 BudgetBlockCard）。 */
export interface BudgetBlock {
  spend_mode: string;
  estimated_cny: number;
  budget_cny: number;
  fresh_viewers: number;
  total_viewers: number;
  hint: string;
}

/** 从 run outcome 析取预算阻断面；无 budget_block 或四数字任一非有限 → null（不渲染卡）。 */
export function budgetBlockOf(outcome: unknown): BudgetBlock | null {
  if (typeof outcome !== "object" || outcome === null || !("budget_block" in outcome)) {
    return null;
  }
  const raw = (outcome as { budget_block?: unknown }).budget_block;
  if (typeof raw !== "object" || raw === null) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const finite = (value: unknown): number | null =>
    typeof value === "number" && Number.isFinite(value) ? value : null;
  const str = (value: unknown): string => (typeof value === "string" ? value : "");
  const estimated_cny = finite(o.estimated_cny);
  const budget_cny = finite(o.budget_cny);
  const fresh_viewers = finite(o.fresh_viewers);
  const total_viewers = finite(o.total_viewers);
  if (
    estimated_cny === null ||
    budget_cny === null ||
    fresh_viewers === null ||
    total_viewers === null
  ) {
    return null;
  }
  return {
    spend_mode: str(o.spend_mode),
    estimated_cny,
    budget_cny,
    fresh_viewers,
    total_viewers,
    hint: str(o.hint),
  };
}

export interface ViewerRow {
  uid: string;
  name: string | null;
  /** 大航海身份面（face 为空串 → null 由服务端 as_str 直接漏空，前端须判空）。 */
  face: string | null;
  /** 大航海等级：3=舰长 / 2=提督 / 1=总督。 */
  guard_level: number | null;
  medal_level: number | null;
  collected_at: string | null;
  ai_status: string | null;
  ai_completed: boolean;
  /** 时效位：true=旧 AI 结论的信源已更新（哈希翻），false=时效内绿灯，null=无参考旧结论。 */
  ai_stale: boolean | null;
  /** 四微件（缺件=null，前端落「未知」微行，绝不补文案/编数字）：
   *  第几次来（WS 场次窗到访计数；无记录=null 而非 0，没有在播窗数据≠没来过）。 */
  visit_count?: number | null;
  /** 距上次 N 天（末次 WS 到场整日数）。 */
  days_since_last?: number | null;
  /** 身份一句（AI 感知 profile_summary 截 40 字；呈现必盖 AI 徽标，不入事实色）。 */
  identity_line?: string | null;
  /** 最新动态日期（名下条目最新 published_at 的 YYYY-MM-DD）。 */
  latest_activity_date?: string | null;
}

/**
 * /rooms/{uid}/overview 响应面（F3 收口：原 request<any> 收窄）。
 * 分段与服务端 app.rs room_overview 的 json! 键一一对应；工件原样透传段
 * （streamer profile / live.records 行 / situation.analysis）保持窄形状，
 * 消费侧沿用 presence 判别，不补齐不存在的数字。
 */
export interface OverviewView {
  room_id?: string;
  streamer_uid?: string;
  project_name?: string;
  /** 主播卡数据面：streamer.json 的 profile 段原样透传；缺文件 → null 空态。 */
  streamer?: StreamerProfile | null;
  /** 直播档案面：shared/live_records.json 透传；records 行形状随 B站接口漂移 → 行级 Record。 */
  live?: {
    status?: string;
    count?: number;
    records?: Array<Record<string, unknown>>;
  } | null;
  /** 图存量指标面（指标条）；无图态 → null（前端显「—」，不臆造数字）。 */
  graph_stats?: Record<string, number> | null;
  /** collection.json 透传（M4.x-T1 schema 冻结读取侧）。 */
  collection?: CollectionView;
  /** ai/state.json 透传（舰长 AI 聚合运行态）。 */
  ai?: AiJobView;
  /** ai/situation.json 透传（audience 段产物）。 */
  situation?: SituationView;
  /** 迭代细则 v1 §1 验收钉：ai/recap.json 透传；缺文件 → null（前端「复盘尚未生成」）。 */
  recap?: import("./components/RecapCard").RecapPayload | null;
  /** BriefingCard refs 归属解析面 episode_id → 归属观众+标题；无图态 → {}。 */
  episode_index?: Record<string, { viewer_id?: string; title?: string | null }>;
  /** G2 表形态读面唯一源（discovery_leads 表）；型单源在 LeadsBlock。 */
  leads?: LeadsView;
  /** 双 run delta（baseline 臂同形）；页面现无消费面板 → 窄 unknown。 */
  delta?: unknown;
}

export interface CollectionView {
  status?: string;
  started_at?: string | null;
  finished_at?: string | null;
  viewer_count?: number;
  /** 合成标示写位（三处分段随工件周期漂移，前端析取不绑定单一来源位）。 */
  leads_consumed?: number;
}

export interface AiJobView {
  status?: string | null;
  completed_at?: string | null;
  /** state.json.usage 原始五键（金额换算只在 format.ts，URL: 单屏单口径）。 */
  usage?: UsageRow | null;
}

export interface SituationView {
  status?: string;
  /** LLM 产物形状漂移（无 schema 校验）——消费侧护栏取值。
   *  deprecated：前台直呈已退役； surviving 唯一消费=BriefingCard 的 front_brief。 */
  analysis?: Record<string, unknown> | null;
}

/** viewers/{uid}.json 原料形状（前进式取用：已知键字面量 + 透传租约）。 */
export interface ViewerPayload {
  schema_version?: number;
  viewer?: { name?: string | null };
  profile?: { name?: string | null; face?: string | null };
  collected_at?: string | null;
}

/** ai/perception/viewers/{uid}.json 缓存形状；analysis 段 LLM 产物 → 消费侧护栏。 */
export interface ViewerAiCache {
  status?: string | null;
  analysis?: Record<string, unknown> | null;
}

/** Episode 时间线行（live-core graph/query.rs EPISODE_COLUMNS 读面形状）。 */
export interface EpisodeRow {
  episode_id: string;
  source?: string;
  event_type?: string;
  observed_at?: string;
  published_at?: string;
  title?: string | null;
  url?: string | null;
  bvid?: string | null;
  fields?: Array<{ path: string; text: string; kind: string }>;
}

/** mention 明细行（mentions_of_viewer 左外联形状 + MentionText 渲染缝）。 */
export interface MentionRow extends MentionSpanLike {
  episode_id?: string;
  field_path?: string;
  origin?: string;
  confidence?: number;
}

/** 个人树面（/tree）：F3 收口——viewer/ai 按服务端组装形状给段，episodes/mentions
 * 行型安在本文件（消费面 ViewerTree.tsx import）。ai_stale 是时效位的确定性面。 */
export interface ViewerTreeView {
  uid: string;
  viewer: ViewerPayload;
  /** 无感知缓存 → null（ai_stale 同位 null）。 */
  ai: ViewerAiCache | null;
  ai_stale: boolean | null;
  episodes: EpisodeRow[];
  mentions: MentionRow[];
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
    /** 第 5 白名单键回显；null = 未设闸（输入框留空 = 保持）。 */
    run_budget_cny: number | null;
    [key: string]: unknown;
  };
  writable_keys: string[];
}

/** GET /api/budget 响应面 = history.jsonl 的月度聚合（UTC 月界）。 */
export interface BudgetLastRun {
  run_id: string;
  ts: string;
  cost_cny: number;
  status: string;
  kind: string;
  spend_mode: string;
}

/** 主钮旁预估段（服务端 estimate_run_cost_cny 上限口径 + 常量墙钟带宽）。
 *  全 null = 名册缺/空/未采集——前端落「预估 —」不臆造。 */
export interface BudgetEstimate {
  roster_viewers: number | null;
  normal_cny: number | null;
  briefing_cny: number | null;
  etd_minutes: [number, number] | null;
}

export interface BudgetInfo {
  /** 单次 run 预算（config ai.run_budget_cny）；null = 不设闸。 */
  budget_cny: number | null;
  /** "YYYY-MM"（UTC 前缀，账本口径与后端 UTC 月界一致）。 */
  month: string;
  month_cost_cny: number;
  month_runs: number;
  last_run: BudgetLastRun | null;
  /** 主钮旁预估（normal_cny/etd_minutes 双空 → 「预估 —」）。 */
  estimate?: BudgetEstimate | null;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  // HTML 错误页/半截代理体曾直接崩 JSON.parse 成裸 SyntaxError 糊脸。
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
  overview: (roomId: string) =>
    request<OverviewView>("GET", `/rooms/${encodeURIComponent(roomId)}/overview`),
  viewers: (roomId: string) =>
    request<ViewerRow[]>("GET", `/rooms/${encodeURIComponent(roomId)}/viewers`),
  viewerTree: (roomId: string, vid: string) =>
    request<ViewerTreeView>(
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
  /** spend_mode 只对带单人感知段的 kind 合法（= /api/runs 校验同源）。 */
  startRun: (body: {
    kind: RunKind;
    force?: boolean;
    viewer_uid?: string;
    spend_mode?: RunSpendMode;
  }) => request<{ run_id: string }>("POST", "/runs", body),
  budget: () => request<BudgetInfo>("GET", "/budget"),
  /** G2-B 审批缝：状态机单行道 pending_approval → approved（幂等；404 未知 / 422 非法迁移）。 */
  approveLead: (roomId: string, leadId: string) =>
    request<{ dedupe_key: string; status: string; changed: boolean }>(
      "POST",
      `/rooms/${encodeURIComponent(roomId)}/leads/${encodeURIComponent(leadId)}/approve`,
    ),
  /** 拒绝缝：状态机单行道 pending_approval → rejected；拒因体可选——
   *  chips 白名单唯一真源在 live-core REJECT_CHIP_REASONS（前端 chip 面 = overview
   *  leads.reject_chip_reasons 下发，不落第二份字面）、note ≤80 字；
   *  全空拒因合法（服务端 NULL/NULL 留档）。幂等重放语义同 approve。 */
  rejectLead: (
    roomId: string,
    leadId: string,
    reasons?: { chips?: string[]; note?: string },
  ) =>
    request<{
      dedupe_key: string;
      status: string;
      changed: boolean;
      reject_chips: string[];
      reject_note: string;
    }>(
      "POST",
      `/rooms/${encodeURIComponent(roomId)}/leads/${encodeURIComponent(leadId)}/reject`,
      reasons,
    ),
};

/** 动作平面：六 kind 字面冻结（与 registry.rs RUN_KINDS/RUN_KINDS_STAGED 同源）。 */
export const RUN_KIND_LABELS = {
  full: "全量感知",
  viewer: "单舰长感知",
  collect_streamer: "主播采集",
  collect_guards: "舰长采集",
  ai_viewers: "舰长 AI 分析",
  ai_audience: "主播 AI 分析",
} as const;

export type RunKind = keyof typeof RUN_KIND_LABELS;

/** spend_mode 字面（= registry SpendMode 的 incremental/briefing_only 两发射极）。 */
export type RunSpendMode = "incremental" | "briefing_only";

/** 服务端 409 错文「已有进行中的 run（{id}），…」的在飞 id 抽取（nil → 纯报错面）。 */
export function activeRunIdFrom(message: string): string | null {
  const hit = /run（([^）]+)）/.exec(message);
  return hit ? hit[1] : null;
}

