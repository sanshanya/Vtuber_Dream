import { useQuery } from "@tanstack/react-query";
import cytoscape from "cytoscape";
import type * as cytoscapeTypes from "cytoscape";
import { useEffect, useRef, useState } from "react";

// @types/cytoscape 是 export= 形如：值走默认导入（可调用），类型走命名空间导入。
// 注：包内 `type Stylesheet = StylesheetStyle | StylesheetCSS` 别名经 namespace
// 通道实测不可达（三种导入形式皆 TS2724）——就地展开并集并留字。
type Stylesheet = cytoscapeTypes.StylesheetStyle | cytoscapeTypes.StylesheetCSS;
type ElementDefinition = cytoscapeTypes.ElementDefinition;

import { api } from "../api";

/** 节点底色：事实/AI/状态/行动四族与 badge 调色对应（styles.css 同源）。 */
const KIND_COLORS: Record<string, string> = {
  Viewer: "#2563eb",
  Entity: "#7c3aed",
  Mention: "#94a3b8",
  Episode: "#0891b2",
  InterestState: "#15803d",
  Situation: "#15803d",
  Action: "#b45309",
};

const EDGE_LABELS: Record<string, string> = {
  INTERESTED_IN: "兴趣",
  GUARD_OF: "舰长",
  REFERS_TO: "指认",
  CONTAINS_MENTION: "含有",
};

/**
 * cose 布局迭代随规模降档（ag5-F9/ag4-F9）：小图跑满收敛即可，大图压档保响应
 * （animate:false 同步语义下 numIter 直接决定主线程阻塞时长）。
 */
const GRAPH_LAYOUT_THRESHOLD_NODES = 500;
const GRAPH_LAYOUT_ITERATIONS_SMALL = 1000;
const GRAPH_LAYOUT_ITERATIONS_LARGE = 400;

/** 节点数 → cose numIter（vitest 唯一定点）。 */
export function graphLayoutIterations(nodeCount: number): number {
  return nodeCount < GRAPH_LAYOUT_THRESHOLD_NODES
    ? GRAPH_LAYOUT_ITERATIONS_SMALL
    : GRAPH_LAYOUT_ITERATIONS_LARGE;
}

function stylePreset(): Stylesheet[] {
  return [
    {
      selector: "node",
      style: {
        label: "data(label)",
        "font-size": 10,
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
      selector: "edge",
      style: {
        label: "data(edge_label)",
        "font-size": 8,
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
      // 关闭的兴趣态：虚线降级（图例在 styles.css legend）。
      selector: 'edge[properties_status = "closed"]',
      style: { "line-style": "dashed", opacity: 0.55 },
    },
    {
      selector: 'edge[predicate = "GUARD_OF"]',
      style: { "line-color": "#15803d", "target-arrow-color": "#15803d" },
    },
    {
      selector: "node:selected",
      style: { "border-width": 2, "border-color": "#dbeafe" },
    },
  ];
}

/**
 * 图谱页（D3：整体 + 局部两用）。elements 来自 cytoscape DTO（B2）；
 * properties 原样透传 → edge_label 与状态压平在 JS 侧做（style 预设吃 data 字段）。
 */
export function GraphPage({ roomId, vid }: { roomId: string; vid?: string }) {
  const graph = useQuery({
    queryKey: ["graph", roomId, vid ?? ""],
    queryFn: () => (vid ? api.viewerGraph(roomId, vid) : api.roomGraph(roomId)),
  });
  const mount = useRef<HTMLDivElement | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [selectedData, setSelectedData] = useState<unknown>(null);

  const elements: unknown[] = Array.isArray(graph.data?.elements) ? graph.data.elements : [];

  useEffect(() => {
    if (!mount.current || elements.length === 0) return;
    // ag4-F9：cy 随新数据重建 → 侧栏选中态必须清，不得残留旧节点 JSON。
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
      style: stylePreset(),
      layout: { name: "cose", numIter: graphLayoutIterations(shaped.length), animate: false },
    });
    cy.on("select", "node", (event) => {
      const node = event.target;
      setSelected(String(node.id()));
      setSelectedData(node.data());
    });
    cy.on("unselect", "node", () => {
      setSelected(null);
      setSelectedData(null);
    });
    return () => cy.destroy();
  }, [elements]);

  if (graph.isLoading) return <div className="empty">载入图谱…</div>;
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
        <div className="legend">
          <span><i className="dot fact" /> Viewer/舰长</span>
          <span><i className="dot ai" /> Entity/兴趣</span>
          <span><i className="dot state" /> 状态</span>
          <span><i className="dot action" /> 行动机会</span>
        </div>
      </div>
      {elements.length === 0 ? (
        <div className="empty">图尚无元素（先跑完 Audience 阶段）</div>
      ) : (
        <div className="graph-layout">
          <div className="card canvas-wrap">
            <div ref={mount} className="graph-canvas" data-testid="graph-canvas" />
          </div>
          <div className="card graph-side">
            <h3>节点详情</h3>
            {selected ? (
              <pre className="protocol">{JSON.stringify(selectedData, null, 2)}</pre>
            ) : (
              <div className="empty">点击节点查看属性与证据</div>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
