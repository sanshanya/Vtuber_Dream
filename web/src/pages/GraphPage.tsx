import { useQuery } from "@tanstack/react-query";
import cytoscape from "cytoscape";
import fcose from "cytoscape-fcose";
import type * as cytoscapeTypes from "cytoscape";
import { Fragment, useEffect, useMemo, useRef, useState } from "react";

// 删码刀13：fcose 布局注册（谱布局播种 + 力导向抛光——cose 单发一锅烩的
// 成熟替代见 iVis-at-Bilkent 仓）。use 幂等注册一次。
cytoscape.use(fcose);

// @types/cytoscape 是 export= 形如：值走默认导入（可调用），类型走命名空间导入。
// 注：包内 `type Stylesheet = StylesheetStyle | StylesheetCSS` 别名经 namespace
// 通道实测不可达（三种导入形式皆 TS2724）——就地展开并集并留字。
type Stylesheet = cytoscapeTypes.StylesheetStyle | cytoscapeTypes.StylesheetCSS;
type ElementDefinition = cytoscapeTypes.ElementDefinition;

import { api } from "../api";

/**
 * 层色：唯一锚点 = styles.css 末尾 :root --layer-* 变量组。
 * 画布不能消费 var()——cytoscape 3.31 实测 `background-color: var(--x)` 判 invalid
 * 并回落基色，故 KIND_COLORS 保留同名变量色值的字面锚（逐行标注对应变量）；
 * DOM 面（详情 kind 徽标等）一律 inline 引用变量（KIND_LAYER_VARS）。
 */
const KIND_COLORS: Record<string, string> = {
  Viewer: "#2563eb", // var(--layer-viewer)
  Entity: "#7c3aed", // var(--layer-entity)
  Mention: "#94a3b8", // var(--layer-mention)
  Episode: "#0891b2", // var(--layer-episode)
  InterestState: "#15803d", // var(--layer-state)
  Situation: "#15803d", // var(--layer-state)
  Action: "#b45309", // var(--layer-action)
};

/** 同色号的 CSS 变量引用形——DOM inline style 用（styles.css 追加段）。 */
export const KIND_LAYER_VARS: Record<string, string> = {
  Viewer: "var(--layer-viewer)",
  Entity: "var(--layer-entity)",
  Mention: "var(--layer-mention)",
  Episode: "var(--layer-episode)",
  InterestState: "var(--layer-state)",
  Situation: "var(--layer-state)",
  Action: "var(--layer-action)",
};

const EDGE_LABELS: Record<string, string> = {
  INTERESTED_IN: "兴趣",
  GUARD_OF: "舰长",
  REFERS_TO: "指认",
  CONTAINS_MENTION: "含有",
};

/**
 * cose 布局迭代随规模降档：小图跑满收敛即可，大图压档保响应
 * （animate:false 同步语义下 numIter 直接决定主线程阻塞时长）。
 */
const GRAPH_LAYOUT_THRESHOLD_NODES = 500;
const GRAPH_LAYOUT_ITERATIONS_SMALL = 1000;
const GRAPH_LAYOUT_ITERATIONS_LARGE = 400;

/** fcose 布局选项单件（init / 过滤重排 / 重排钮 同源；vitest 经 numIter 定点）。 */
function fcoseOptions(nodeCount: number) {
  return {
    name: "fcose",
    quality: "default",
    numIter: graphLayoutIterations(nodeCount),
    animate: false,
    idealEdgeLength: nodeCount > GRAPH_LAYOUT_THRESHOLD_NODES ? 90 : 70,
    nodeRepulsion: 20000,
  } as const;
}

/** 节点数 → cose numIter（vitest 唯一定点）。 */
export function graphLayoutIterations(nodeCount: number): number {
  return nodeCount < GRAPH_LAYOUT_THRESHOLD_NODES
    ? GRAPH_LAYOUT_ITERATIONS_SMALL
    : GRAPH_LAYOUT_ITERATIONS_LARGE;
}

/** 主播锚点颜色：橘黄（用户裁决 2026-08-06；var() 画布不吃，字面锚同步 styles.css）。 */
const STREAMER_COLOR = "#ea580c";

/**
 * 主播锚标记：唯一权威锚 = 平台事实 creator 实体
 * （id = `entity:creator:{streamer_uid}`，identity 来自 B 站本身，非 AI 推断）。
 * 鉴注：观众各自对主播的 AI 变体实体（36 份）与 creator 锚的 SAME_AS 合流
 * 是实体解析欠账（已挂 backlog），本标记只锚平台事实，不糊成一锅。
 */
export function markStreamer(elements: unknown[], streamerUid: string): unknown[] {
  if (!streamerUid) return elements;
  const anchorId = `entity:creator:${streamerUid}`;
  return (elements as Array<{ data?: Record<string, unknown> }>).map((element) => {
    const data = element?.data ?? {};
    if (String(data.id) === anchorId) {
      return { ...element, data: { ...data, streamer: true } };
    }
    return element;
  });
}

// ---------------------------------------------------------------------------
// 节点详情结构化
// ---------------------------------------------------------------------------

export interface NodeDetailRow {
  label: string;
  value: string;
}

/** 身份信号：由图边推出（OWNS_ROOM 源=主播；GUARD_OF 源=舰长；其余观众）。 */
export interface NodeIdentitySignals {
  streamer?: boolean;
  guard?: boolean;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function present(value: unknown): string {
  if (typeof value === "string") return value.trim();
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  if (Array.isArray(value)) {
    return value
      .map((item) => present(item))
      .filter((item) => item.length > 0)
      .join("、");
  }
  return "";
}

/** 已知 kind 的语义键值序（presence 直出：值为空该行不渲）。 */
const KIND_DETAIL_KEYS: Record<string, Array<[string, string]>> = {
  Entity: [
    ["entity_type", "类型"],
    ["description", "描述"],
    ["identity_source", "标识源"],
    ["platform_id", "平台标识"],
    ["platform_value", "平台值"],
  ],
  Mention: [
    ["episode_id", "属场次"],
    ["field_path", "出处字段"],
    ["origin", "来源"],
  ],
  Episode: [
    ["event_type", "事件类型"],
    ["published_at", "发布时间"],
    ["url", "链接"],
  ],
  InterestState: [
    ["direction", "方向"],
    ["status", "状态"],
    ["preference", "取向"],
    ["aspects", "关注角度"],
    ["rationale", "依据"],
    ["evidence_summary", "evidence 摘要"],
  ],
  Situation: [
    ["status", "状态"],
    ["description", "描述"],
    ["confidence", "置信度"],
    ["entities", "涉及实体"],
    ["viewer_ids", "涉及观众"],
    ["trigger_events", "触发事件"],
    ["recommended_investigation", "追踪建议"],
  ],
  Action: [
    ["format", "形式"],
    ["entity", "实体"],
    ["why_now", "为何现在"],
    ["why_fit", "为何契合"],
    ["audience_ids", "目标观众"],
    ["confidence", "置信度"],
    ["run_of_show", "流程"],
    ["talking_points", "讨论点"],
    ["caveats", "注意"],
  ],
};

/**
 * 节点 → 结构化键值行。已知 kind 按语义键序 presence 出列（Viewer 自带 uid/名字/身份）；
 * 未知 kind 或出列为空 → null（消费面降级 <details> JSON 兜底）。
 */
export function describeNode(
  data: unknown,
  signals: NodeIdentitySignals = {},
): NodeDetailRow[] | null {
  const record = asRecord(data);
  const kind = typeof record.kind === "string" ? record.kind : "";
  const label = typeof record.label === "string" ? record.label : "";
  const properties = asRecord(record.properties);
  const id = typeof record.id === "string" ? record.id : "";
  const rows: NodeDetailRow[] = [];
  if (kind === "Viewer") {
    const uid = present(properties.viewer_id) || id.replace(/^viewer:/, "");
    if (uid) rows.push({ label: "uid", value: uid });
    if (label) rows.push({ label: "名字", value: label });
    rows.push({
      label: "身份",
      value:
        present(properties.identity) || (signals.streamer ? "主播" : signals.guard ? "舰长" : "观众"),
    });
    return rows;
  }
  const keys = KIND_DETAIL_KEYS[kind];
  if (!keys) return null;
  if (label) rows.push({ label: "名称", value: label });
  for (const [key, name] of keys) {
    const value = present(properties[key]);
    if (value) rows.push({ label: name, value });
  }
  return rows.length > 0 ? rows : null;
}

/** 社区环调色板：community_id 稳定哈希取色（kind 填充色是事实/AI 分层语义，
 *  环色只做编组提示，绝不冲层色）。 */
const COMMUNITY_PALETTE = [
  "#f59e0b", "#10b981", "#38bdf8", "#f472b6", "#a3e635", "#fb7185",
  "#22d3ee", "#e879f9", "#facc15", "#34d399", "#93c5fd", "#fda4af",
];
function communityColor(communityId: unknown): string {
  const text = typeof communityId === "string" ? communityId : "";
  let hash = 0;
  for (const ch of text) hash = (hash * 31 + (ch.codePointAt(0) ?? 0)) >>> 0;
  return COMMUNITY_PALETTE[hash % COMMUNITY_PALETTE.length];
}

/** style 预设。isEgo=true（局部图）保留全员标签（一跳邻域规模可读）；
 *  整体图启用 degree LOD（删码刀13 去岩浆三件之一：degree<2 默认隐名，
 *  悬停/选中恒显——选中/悬停规则排位在此 LOD 之后，后写赢）。 */
function stylePreset(isEgo: boolean): Stylesheet[] {
  return [
    {
      selector: "node",
      style: {
        label: "data(label)",
        "font-size": 10,
        // 2026-08-05 用户裁决：全场适配缩放下全展开标签 = 渲染岩浆（且与 251→2522
        // 时代同属「好玩吗」噪声）——字体渲染小于 9px 时整体隐藏，放大自然显名。
        "min-zoomed-font-size": 9,
        color: "#8b949e",
        "text-valign": "bottom",
        "text-margin-y": 4,
        "text-wrap": "wrap",
        "text-max-width": "120px",
        width: 24,
        height: 24,
        "background-color": "#94a3b8",
      },
    },
    ...(isEgo
      ? []
      : ([
          {
            // 删码刀13：degree LOD——低邻居节点默认隐名（整体图首屏去岩浆主力）。
            selector: "node[degree < 2]",
            style: { label: "" },
          } as Stylesheet,
        ] as Stylesheet[])),
    ...Object.entries(KIND_COLORS).map(
      ([kind, color]): Stylesheet => ({
        selector: `node[kind = "${kind}"]`,
        style: {
          "background-color": color,
          width: kind === "Viewer" ? 34 : 22,
          height: kind === "Viewer" ? 34 : 22,
        },
      }),
    ),
    {
      // 2026-08-06 用户裁决：主播锚橘黄高亮——旗由 markStreamer 打，样式只认 flag。
      selector: "node[streamer]",
      style: {
        "background-color": STREAMER_COLOR,
        width: 34,
        height: 34,
        "border-width": 2,
        "border-color": "#fbbf24",
      },
    },
    {
      // 社区环（删码刀13）：CNM 编组提示——data 函数逐节点取调色板色。
      selector: "node[community_id]",
      style: {
        "border-width": 2,
        "border-opacity": 0.85,
        "border-color": (ele: { data: (key: string) => unknown }) =>
          communityColor(ele.data("community_id")),
      },
    },
    {
      selector: "edge",
      style: {
        // 删码刀13：边标签默认隐——只留高置信与选中（岩浆二号凶手伏法）。
        label: "",
        "font-size": 8,
        "min-zoomed-font-size": 7,
        color: "#8b949e",
        "text-rotation": "autorotate",
        width: 1,
        "line-color": "#30363d",
        "target-arrow-shape": "triangle",
        "target-arrow-color": "#30363d",
        "arrow-scale": 0.8,
        "curve-style": "bezier",
      },
    },
    {
      selector: 'edge[predicate = "INTERESTED_IN"]',
      style: {
        "line-color": "#7c3aed",
        "target-arrow-color": "#7c3aed",
        width: "mapData(confidence, 0, 1, 1, 4)",
      },
    },
    {
      selector: "edge[confidence >= 0.8], edge:selected",
      style: { label: "data(edge_label)" },
    },
    {
      // 关闭的兴趣态：虚线降级（图例在 styles.css legend）。
      selector: 'edge[properties_status = "closed"]',
      style: { "line-style": "dashed", opacity: 0.55 },
    },
    {
      selector: 'edge[predicate = "GUARD_OF"]',
      style: { "line-color": "#15803d", "target-arrow-color": "#15803d" },
    },
    {
      // 悬停显名（LOD 之后落位——后写赢隐名规则）。
      selector: "node.hover, node:selected",
      style: { label: "data(label)" },
    },
    {
      selector: "node:selected",
      style: { "border-width": 2, "border-color": "#dbeafe" },
    },
    {
      // hover 邻域聚焦：非邻域降暗。
      selector: ".dimmed",
      style: { opacity: 0.12 },
    },
    {
      // 过滤钳（工具条三件套：kind 开关 / 置信度滑杆 共用此类）。
      selector: ".filtered",
      style: { display: "none" },
    },
  ];
}

/**
 * 图谱页（整体 + 局部两用）。elements 来自 cytoscape DTO；
 * properties 原样透传 → edge_label 与状态压平在 JS 侧做（style 预设吃 data 字段）。
 * - 节点详情面按已知 kind 渲结构化键值（describeNode），兜底才 <details> JSON；
 * - a11y：canvas 外叠 hidden-but-focusable 节点清单（.graph-a11y）——li 可聚焦，
 *   Enter 选中节点（与 canvas 点选同路：cy.select → select 事件 → 详情面）。
 */
export function GraphPage({ roomId, vid }: { roomId: string; vid?: string }) {
  const graph = useQuery({
    queryKey: ["graph", roomId, vid ?? ""],
    queryFn: () => (vid ? api.viewerGraph(roomId, vid) : api.roomGraph(roomId)),
  });
  // 主播锚上色数据源：rooms 已被 App 预取缓存——uid 找主播不花一分新请求。
  const rooms = useQuery({ queryKey: ["rooms"], queryFn: api.rooms });
  const mount = useRef<HTMLDivElement | null>(null);
  const cyRef = useRef<cytoscapeTypes.Core | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [selectedData, setSelectedData] = useState<unknown>(null);

  // useMemo 把 elements 钉在稳定内容依赖上——裸 markStreamer 每次
  // render 返新数组引用，下游 useEffect([elements]) 对纯内容变化照触发，
  // 导致每次点击整 cytoscape.destroy()+重建+清选中（「点节点看详情」曾因此失效）。
  const streamerUid = rooms.data?.[0]?.streamer_uid ?? "";
  const rawElements = Array.isArray(graph.data?.elements) ? graph.data.elements : [];
  const elements: unknown[] = useMemo(
    () => markStreamer(rawElements, streamerUid),
    [rawElements, streamerUid],
  );

  // 删码刀13 工具条三件套状态（过滤钳共用 filtered 类——见 stylePreset 尾）。
  const [hiddenKinds, setHiddenKinds] = useState<ReadonlySet<string>>(new Set());
  const [minConfidence, setMinConfidence] = useState(0);
  const [search, setSearch] = useState("");

  useEffect(() => {
    if (!mount.current || elements.length === 0) return;
    // cy 随新数据重建 → 侧栏选中态必须清，不得残留旧节点 JSON。
    setSelected(null);
    setSelectedData(null);
    const shaped: ElementDefinition[] = (elements as any[]).map((element) => {
      const data = { ...(element?.data ?? {}) };
      if (typeof data.predicate === "string") {
        data.edge_label = EDGE_LABELS[data.predicate] ?? data.predicate;
      }
      if (data.properties && typeof data.properties === "object") {
        const status = (data.properties as Record<string, unknown>).status;
        if (typeof status === "string" && data.properties_status === undefined) {
          data.properties_status = status;
        }
      }
      return { data };
    });
    const cy = cytoscape({
      container: mount.current,
      elements: shaped,
      style: stylePreset(Boolean(vid)),
      layout: fcoseOptions(shaped.length),
    });
    cyRef.current = cy;
    cy.on("select", "node", (event) => {
      const node = event.target;
      setSelected(String(node.id()));
      setSelectedData(node.data());
    });
    cy.on("unselect", "node", () => {
      setSelected(null);
      setSelectedData(null);
    });
    // hover 邻域聚焦：非邻域降暗 + 悬停显名（LOD 隐名规则的悬停豁免）。
    cy.on("mouseover", "node", (event) => {
      const node = event.target;
      const neighborhood = node.closedNeighborhood();
      cy.elements()
        .not(neighborhood)
        .forEach((el) => {
          el.addClass("dimmed");
        });
      node.addClass("hover");
    });
    cy.on("mouseout", "node", () => {
      cy.elements().forEach((el) => {
        el.removeClass("dimmed");
      });
      cy.nodes().forEach((el) => {
        el.removeClass("hover");
      });
    });
    return () => {
      cyRef.current = null;
      cy.destroy();
    };
  }, [elements]);

  // 过滤钳施加器：kind 开关 + 置信度滑杆改完即刻落 filtered 类并重排可见集
  // （隐藏节点不占 fcose 位——过滤就是排布漏斗，不是遮罩）。
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    cy.batch(() => {
      cy.nodes().forEach((node) => {
        node.toggleClass("filtered", hiddenKinds.has(String(node.data("kind"))));
      });
      cy.edges().forEach((edge) => {
        const confidence = edge.data("confidence");
        const cutoff =
          typeof confidence === "number" && confidence * 100 < minConfidence;
        edge.toggleClass("filtered", cutoff);
      });
    });
    cy.layout(fcoseOptions(elements.length)).run();
  }, [elements, hiddenKinds, minConfidence]);

  // 节点清单（边不入册）与身份推导（边谓词 → 角色信号）。
  const nodeIndex = elements
    .map((element) => asRecord(asRecord(element).data))
    .filter((data) => typeof data.kind === "string" && typeof data.id === "string")
    .map((data) => ({ id: data.id as string, kind: data.kind as string, label: typeof data.label === "string" ? data.label : "" }));
  const edgeData = elements
    .map((element) => asRecord(asRecord(element).data))
    .filter((data) => typeof data.predicate === "string");
  const identitySignals = (nodeId: string): NodeIdentitySignals => ({
    streamer: edgeData.some((d) => d.predicate === "OWNS_ROOM" && d.source === nodeId),
    guard: edgeData.some((d) => d.predicate === "GUARD_OF" && d.source === nodeId),
  });
  const selectNode = (nodeId: string) => {
    const target = cyRef.current?.$id(nodeId);
    if (target && target.length > 0) {
      // 与 canvas 同路：select() 触发 cy 的 select 事件 → 详情面。
      target.select();
      return;
    }
    // cy 缺席（mock/竞态）的降级径：就地取用 elements 数据同步详情面。
    const element = (elements as any[]).find((e) => asRecord(e?.data).id === nodeId);
    if (element) {
      setSelected(nodeId);
      setSelectedData(asRecord(element).data);
    }
  };
  const selectedKind =
    selectedData && typeof asRecord(selectedData).kind === "string"
      ? (asRecord(selectedData).kind as string)
      : "";
  const detailRows = selected ? describeNode(selectedData, identitySignals(selected)) : null;

  // 工具条出件——present kinds 自动成席；搜索定位走 a11y 同一路（selectNode）。
  const kinds = [...new Set(nodeIndex.map((node) => node.kind))].sort();
  const visibleNodeIndex = nodeIndex.filter((node) => !hiddenKinds.has(node.kind));
  const toggleKind = (kind: string) => {
    setHiddenKinds((previous) => {
      const next = new Set(previous);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });
  };
  const locate = () => {
    const needle = search.trim().toLowerCase();
    if (!needle) return;
    const hit = nodeIndex.find((node) => node.label.toLowerCase().includes(needle));
    if (!hit) return;
    selectNode(hit.id);
    const cy = cyRef.current;
    const target = cy?.$id(hit.id);
    if (cy && target && target.length > 0) cy.center(target);
  };

  if (graph.isLoading) return <div className="state-loading">载入图谱…</div>;
  if (graph.isError)
    return (
      <div className="notice">
        {String(graph.error instanceof Error ? graph.error.message : graph.error)}
      </div>
    );

  return (
    <section className="section">
      <div className="section-title">
        <h2>{vid ? `局部图 · ${vid}` : "整体图谱"}</h2>
        {/* 默认折叠视图如实标注——细节层（场次/证据）不进首载。 */}
        {!vid && (
          <span className="muted small" data-testid="graph-fold-hint">
            默认折叠视图：仅人与关注点主骨架
          </span>
        )}
        {/* 状态/行动机会图例行清退——简报已承接态势展示面，图例只解
            说当前实有节点类（Situation/Action 节点机制仍在，复出以色辨位）。 */}
        <div className="legend">
          <span><i className="dot fact" /> Viewer/舰长</span>
          <span><i className="dot ai" /> Entity/兴趣</span>
          <span><i className="dot" style={{ background: STREAMER_COLOR }} /> 主播</span>
        </div>
      </div>
      {elements.length === 0 ? (
        <div className="empty">图尚无元素（先跑完 Audience 阶段）</div>
      ) : (
        <>
          <div className="graph-toolbar card" data-testid="graph-toolbar">
            <input
              data-testid="graph-search"
              placeholder="搜节点名，回车定位"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") locate();
              }}
            />
            <span className="chips">
              {kinds.map((kind) => (
                <button
                  key={kind}
                  type="button"
                  className={`chip${hiddenKinds.has(kind) ? " off" : ""}`}
                  data-testid={`graph-kind-${kind}`}
                  aria-pressed={!hiddenKinds.has(kind)}
                  onClick={() => toggleKind(kind)}
                >
                  {kind}
                </button>
              ))}
            </span>
            <label className="graph-conf">
              置信度 ≥
              <input
                type="range"
                min={0}
                max={100}
                step={5}
                value={minConfidence}
                data-testid="graph-conf-range"
                onChange={(event) => setMinConfidence(Number(event.target.value))}
              />
              <span data-testid="graph-conf-value">{minConfidence}</span>
            </label>
            <button
              type="button"
              data-testid="graph-relayout"
              onClick={() => cyRef.current?.layout(fcoseOptions(elements.length)).run()}
            >
              重排
            </button>
            <button
              type="button"
              data-testid="graph-fit"
              onClick={() => cyRef.current?.fit()}
            >
              复位
            </button>
            <span className="muted small">
              {visibleNodeIndex.length}/{nodeIndex.length} 节点
            </span>
          </div>
          <div className="graph-layout">
            <div className="card canvas-wrap">
              <div ref={mount} className="graph-canvas" data-testid="graph-canvas" />
              {/* canvas 本身不可聚焦——外叠一份 hidden-but-focusable 节点清单（Enter = 点选）。 */}
              <ul className="graph-a11y" data-testid="graph-a11y" aria-label="图节点清单（回车选中查看详情）">
                {visibleNodeIndex.map((node) => (
                <li
                  key={node.id}
                  tabIndex={0}
                  aria-label={`节点 ${node.label || node.id}（${node.kind}），回车选中`}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") selectNode(node.id);
                  }}
                  onClick={() => selectNode(node.id)}
                >
                  {node.label || node.id}（{node.kind}）
                </li>
              ))}
            </ul>
          </div>
          <div className="card graph-side">
            <h3>节点详情</h3>
            {selected ? (
              <div data-testid="graph-node-detail">
                {selectedKind ? (
                  <span
                    className="badge graph-kind-chip"
                    data-testid="graph-kind-chip"
                    style={
                      KIND_LAYER_VARS[selectedKind]
                        ? { background: KIND_LAYER_VARS[selectedKind] }
                        : undefined
                    }
                  >
                    {selectedKind}
                  </span>
                ) : null}
                {detailRows ? (
                  <dl className="kv">
                    {detailRows.map((row) => (
                      <Fragment key={row.label}>
                        <dt>{row.label}</dt>
                        <dd>{row.value}</dd>
                      </Fragment>
                    ))}
                  </dl>
                ) : (
                  // 未知 kind 或语义键全缺席才走 JSON 兜底。
                  <details data-testid="graph-node-raw">
                    <summary>原始属性（未识别类型）</summary>
                    <pre className="protocol">{JSON.stringify(selectedData, null, 2)}</pre>
                  </details>
                )}
              </div>
            ) : (
              <div className="empty">点击节点查看属性与证据</div>
            )}
          </div>
        </div>
        </>
      )}
    </section>
  );
}
