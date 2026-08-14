/**
 * GraphPage 钉团
 * - 节点详情结构化（已知 kind 渲语义键值行；未知 kind / 键全缺席 → <details> JSON 兜底）；
 * - a11y（图谱部分）：canvas 外叠 hidden-but-focusable 节点清单，
 *   li 可聚焦、Enter 选中节点 → 详情面联动；
 * - 详情 kind 徽标底色引用 styles.css 层色变量 var(--layer-*)（画布不能消费 var()，
 *   DOM 面必须走变量引用，不再各写各的色号）。
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mockCy = vi.hoisted(() => ({
  handlers: {} as Record<string, (event: any) => void>,
  selectedCalls: [] as string[],
  inits: 0,
  lastInit: null as any,
  layoutRuns: [] as string[],
  useCalls: 0,
}));

vi.mock("cytoscape", () => {
  const collectionStub: any = {
    forEach: () => collectionStub,
    toggleClass: () => collectionStub,
    addClass: () => collectionStub,
    removeClass: () => collectionStub,
    filter: () => collectionStub,
    not: () => collectionStub,
    length: 0,
  };
  const cytoscapeFn: any = (opts: any) => {
    mockCy.inits += 1;
    mockCy.lastInit = opts;
    return {
      on: (evt: string, selector: string, cb: (event: any) => void) => {
        mockCy.handlers[`${evt}:${selector}`] = cb;
      },
      destroy: () => {},
      $id: (id: string) => ({
        length: 1,
        select: () => {
          mockCy.selectedCalls.push(id);
          mockCy.handlers["select:node"]?.({
            target: { id: () => id, data: () => DATA_BY_ID[id] },
          });
        },
      }),
      layout: (opts: any) => ({
        run: () => {
          mockCy.layoutRuns.push(String(opts?.name ?? ""));
        },
      }),
      fit: () => {},
      center: () => {},
      batch: (fn: () => void) => fn(),
      nodes: () => collectionStub,
      edges: () => collectionStub,
      elements: () => collectionStub,
    };
  };
  cytoscapeFn.use = () => {
    mockCy.useCalls += 1;
  };
  return { __esModule: true, default: cytoscapeFn };
});

import { describeNode, GraphPage, graphLayoutIterations, markStreamer } from "../pages/GraphPage";

const NODE_VIEWER = {
  data: { id: "viewer:demo-1", label: "演示观众A", kind: "Viewer", properties: { viewer_id: "demo-1" } },
};
const NODE_STREAMER = {
  data: { id: "viewer:9001", label: "9001", kind: "Viewer", properties: { viewer_id: "9001" } },
};
const NODE_ENTITY = {
  data: {
    id: "entity:game:e1",
    label: "异环",
    kind: "Entity",
    properties: { entity_type: "game", description: "开放都市 RPG" },
  },
};
const NODE_WEIRD = { data: { id: "weird:1", label: "怪节点", kind: "Weird", properties: {} } };
const EDGE_GUARD = {
  data: { id: "edge:g", source: "viewer:demo-1", target: "viewer:9001", predicate: "GUARD_OF" },
};
const EDGE_OWN = {
  data: {
    id: "edge:o",
    source: "viewer:9001",
    target: "room:983",
    predicate: "OWNS_ROOM",
    confidence: 1,
  },
};
const EDGE_INTEREST = {
  data: {
    id: "edge:i",
    source: "viewer:demo-1",
    target: "entity:game:e1",
    predicate: "INTERESTED_IN",
    confidence: 0.8,
    properties: { status: "active" },
  },
};

const NODES = [NODE_VIEWER, NODE_STREAMER, NODE_ENTITY, NODE_WEIRD];
const ELEMENTS = [...NODES, EDGE_GUARD, EDGE_OWN, EDGE_INTEREST];
const DATA_BY_ID: Record<string, unknown> = Object.fromEntries(
  NODES.map((n) => [n.data.id, n.data]),
);

function stubFetch(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, text: async () => JSON.stringify(body) }) as Response),
  );
}

function renderGraph() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
  render(
    <QueryClientProvider client={queryClient}>
      <GraphPage roomId="983" />
    </QueryClientProvider>,
  );
}

/** 模拟 canvas 点选（cytoscape select 事件）。 */
function emitSelect(id: string) {
  act(() => {
    mockCy.handlers["select:node"]({ target: { id: () => id, data: () => DATA_BY_ID[id] } });
  });
}

beforeEach(() => {
  mockCy.handlers = {};
  mockCy.selectedCalls = [];
  mockCy.inits = 0;
  mockCy.lastInit = null;
  mockCy.layoutRuns = [];
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("FE-F2：节点详情结构化", () => {
  it("Viewer：uid/名字/身份（GUARD_OF 推舰长）结构化行 + kind 徽标走层色变量", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => expect(screen.getByTestId("graph-a11y")).toBeTruthy());
    emitSelect("viewer:demo-1");
    const detail = screen.getByTestId("graph-node-detail");
    expect(detail.textContent).toContain("uid");
    expect(detail.textContent).toContain("demo-1");
    expect(detail.textContent).toContain("名字");
    expect(detail.textContent).toContain("演示观众A");
    expect(detail.textContent).toContain("身份");
    expect(detail.textContent).toContain("舰长");
    // 无 JSON 兜底块
    expect(screen.queryByTestId("graph-node-raw")).toBeNull();
    // kind 徽标：背景引用层色变量，不再硬写色号
    const chip = screen.getByTestId("graph-kind-chip");
    expect(chip.getAttribute("style")).toContain("var(--layer-viewer)");
    expect(chip.textContent).toBe("Viewer");
  });

  it("主播身份由 OWNS_ROOM 推出；无身份边 → 观众", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => expect(screen.getByTestId("graph-a11y")).toBeTruthy());
    emitSelect("viewer:9001");
    expect(screen.getByTestId("graph-node-detail").textContent).toContain("主播");
  });

  it("Entity：名称/类型/描述行", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => expect(screen.getByTestId("graph-a11y")).toBeTruthy());
    emitSelect("entity:game:e1");
    const detail = screen.getByTestId("graph-node-detail");
    expect(detail.textContent).toContain("名称");
    expect(detail.textContent).toContain("异环");
    expect(detail.textContent).toContain("类型");
    expect(detail.textContent).toContain("game");
    expect(detail.textContent).toContain("描述");
    expect(detail.textContent).toContain("开放都市 RPG");
    expect(screen.queryByTestId("graph-node-raw")).toBeNull();
  });

  it("未知 kind / 键全缺席 → <details> JSON 兜底", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => expect(screen.getByTestId("graph-a11y")).toBeTruthy());
    emitSelect("weird:1");
    const raw = screen.getByTestId("graph-node-raw");
    expect(raw.tagName).toBe("DETAILS");
    expect(raw.textContent).toContain("weird:1");
  });
});

describe("FE-F2：a11y 节点清单（R3#9 图谱部分）", () => {
  it("canvas 外叠 hidden-but-focusable 清单：仅节点、 li 可聚焦、Enter 选中联动详情面", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => {
      const list = screen.getByTestId("graph-a11y");
      expect(list.tagName).toBe("UL");
      expect(list.querySelectorAll("li").length).toBe(4);
    });
    const list = screen.getByTestId("graph-a11y");
    const items = Array.from(list.querySelectorAll("li"));
    for (const li of items) {
      expect(li.tabIndex).toBe(0);
    }
    // 选中实体节点（第 3 个节点元素）
    const entityItem = items.find((li) => li.textContent?.includes("异环"))!;
    fireEvent.keyDown(entityItem, { key: "Enter" });
    expect(mockCy.selectedCalls).toContain("entity:game:e1");
    expect(screen.getByTestId("graph-node-detail").textContent).toContain("开放都市 RPG");
  });
});

describe("FE-F2：describeNode 单钉（kind → 语义键值行）", () => {
  it("InterestState：方向/evidence 摘要行", () => {
    const rows = describeNode({
      kind: "InterestState",
      label: "异环",
      properties: { direction: "深入", evidence_summary: "两名观众独立收藏" },
    });
    expect(rows).not.toBeNull();
    const text = JSON.stringify(rows);
    expect(text).toContain("方向");
    expect(text).toContain("深入");
    expect(text).toContain("evidence 摘要");
    expect(text).toContain("两名观众独立收藏");
  });

  it("Action：分段键值行（形式/为何现在/置信度…）", () => {
    const rows = describeNode({
      kind: "Action",
      label: "素材观看",
      properties: {
        format: "素材观看+投票",
        why_now: "信号汇聚",
        confidence: "高",
        audience_ids: ["demo-1", "demo-2"],
      },
    });
    const text = JSON.stringify(rows);
    expect(text).toContain("形式");
    expect(text).toContain("素材观看+投票");
    expect(text).toContain("为何现在");
    expect(text).toContain("信号汇聚");
    expect(text).toContain("置信度");
    expect(text).toContain("demo-1、demo-2");
  });

  it("Situation：状态/描述/置信度行", () => {
    const rows = describeNode({
      kind: "Situation",
      label: "共同讨论入口",
      properties: { status: "新出现", description: "两个角度", confidence: 0.9 },
    });
    const text = JSON.stringify(rows);
    expect(text).toContain("状态");
    expect(text).toContain("新出现");
    expect(text).toContain("0.9");
  });

  it("未知 kind / 已知 kind 键全缺席 → null（交兜底）", () => {
    expect(describeNode({ kind: "Weird", label: "x", properties: { a: 1 } })).toBeNull();
    expect(describeNode({ kind: "Entity", properties: {} })).toBeNull();
    expect(describeNode(null)).toBeNull();
  });
});

describe("cose numIter 随规模降档", () => {
  it("分界 500：以下 1000（小图收敛优先），以上 400（大图响应优先）", () => {
    expect(graphLayoutIterations(0)).toBe(1000);
    expect(graphLayoutIterations(499)).toBe(1000);
    expect(graphLayoutIterations(500)).toBe(400);
    expect(graphLayoutIterations(50_000)).toBe(400);
  });
});

describe("2026-08-06 用户裁决：主播锚点橘黄标记", () => {
  it("平台 creator 锚（entity:creator:{uid}）打 streamer 旗，其余节点不动", () => {
    const elements = [
      { data: { id: "entity:creator:3546595083683995", label: "芜湖奶芙一溺爱版", kind: "Entity", properties: { platform_id: "3546595083683995" } } },
      { data: { id: "entity:虚拟主播_up主:12202a04", label: "芜湖奶芙一溺爱版", kind: "Entity", properties: {} } },
      { data: { id: "viewer:demo-1", label: "演示观众A", kind: "Viewer", properties: {} } },
    ];
    const marked = markStreamer(elements, "3546595083683995") as any[];
    expect(marked[0].data.streamer).toBe(true);
    expect(marked[1].data.streamer).toBeUndefined();
    expect(marked[2].data.streamer).toBeUndefined();
    // uid 缺位 → 原样返回，不臆造锚点。
    expect(markStreamer(elements, "")).toEqual(elements);
  });
});

describe("Z6/P0-6：整体图折叠提示", () => {
  it("整体图谱（无 vid）呈现默认折叠提示；局部图（有 vid）不呈现", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await screen.findByTestId("graph-fold-hint");
    expect(screen.getByTestId("graph-fold-hint").textContent).toContain("默认折叠视图");
  });
});

describe("删码刀13：fcose + LOD + 工具条", () => {
  it("布局 = fcose；整体图 style 含 degree<2 LOD 与 .dimmed/.filtered 规则", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => expect(screen.getByTestId("graph-a11y")).toBeTruthy());
    expect(mockCy.lastInit.layout.name).toBe("fcose");
    const selectors = (mockCy.lastInit.style as Array<{ selector?: string }>)
      .map((rule) => rule.selector)
      .join("|");
    expect(selectors).toContain("node[degree < 2]");
    expect(selectors).toContain(".dimmed");
    expect(selectors).toContain(".filtered");
    expect(selectors).toContain("node[community_id]");
  });

  it("ego 局部图小规模（<60 节点）不加 degree<2 LOD（一跳邻域规模本就显名）", async () => {
    stubFetch({ elements: ELEMENTS });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
    render(
      <QueryClientProvider client={queryClient}>
        <GraphPage roomId="983" vid="demo-1" />
      </QueryClientProvider>,
    );
    await waitFor(() => expect(screen.getByTestId("graph-a11y")).toBeTruthy());
    const selectors = (mockCy.lastInit.style as Array<{ selector?: string }>)
      .map((rule) => rule.selector)
      .join("|");
    expect(selectors).not.toContain("node[degree < 2]");
  });

  it("ego 局部图 ≥60 节点同样上 degree<2 LOD（14213360 星形 118 节点实证成汤）", async () => {
    // 61 片叶子挂一根 hub——star 拓扑的大 ego 局部图。
    const hub = { data: { id: "viewer:hub", label: "14213360", kind: "Viewer", degree: 61, properties: { viewer_id: "hub" } } };
    const leaves = Array.from({ length: 61 }, (_, i) => ({
      data: { id: `entity:game:e${i}`, label: `实体${i}`, kind: "Entity", degree: 1, properties: {} },
    }));
    const edges = leaves.map((leaf, i) => ({
      data: { id: `edge:e${i}`, source: "viewer:hub", target: leaf.data.id, predicate: "INTERESTED_IN", confidence: 0.9 },
    }));
    stubFetch({ elements: [hub, ...leaves, ...edges] });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: 0 } } });
    render(
      <QueryClientProvider client={queryClient}>
        <GraphPage roomId="983" vid="hub" />
      </QueryClientProvider>,
    );
    await waitFor(() => expect(screen.getByTestId("graph-a11y")).toBeTruthy());
    const selectors = (mockCy.lastInit.style as Array<{ selector?: string }>)
      .map((rule) => rule.selector)
      .join("|");
    expect(selectors).toContain("node[degree < 2]");
  });

  it("工具条：kind 芯片开关（off 态 + aria-pressed 翻转 + 清单计数收窄）与滑杆改值", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => expect(screen.getByTestId("graph-toolbar")).toBeTruthy());
    // present kinds 自动成席（Viewer/Entity/Weird 三席）。
    const viewerChip = screen.getByTestId("graph-kind-Viewer");
    expect(viewerChip.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(viewerChip);
    expect(screen.getByTestId("graph-kind-Viewer").getAttribute("aria-pressed")).toBe("false");
    expect(screen.getByTestId("graph-kind-Viewer").className).toContain("off");
    // 清单计数跟随（Viewer 席下有两席 demo-1/9001 → 隐 2 剩 2/4）与 a11y 列表同窄。
    expect(screen.getByTestId("graph-toolbar").textContent).toContain("2/4 节点");
    // 滑杆改值回显。
    fireEvent.change(screen.getByTestId("graph-conf-range"), { target: { value: "60" } });
    expect(screen.getByTestId("graph-conf-value").textContent).toBe("60");
    // 重排钮再走一次 fcose。
    mockCy.layoutRuns = [];
    fireEvent.click(screen.getByTestId("graph-relayout"));
    expect(mockCy.layoutRuns).toContain("fcose");
  });

  it("搜索回车定位：contains 命中首件 → 走 a11y 同一路 select", async () => {
    stubFetch({ elements: ELEMENTS });
    renderGraph();
    await waitFor(() => expect(screen.getByTestId("graph-search")).toBeTruthy());
    fireEvent.change(screen.getByTestId("graph-search"), { target: { value: "异环" } });
    fireEvent.keyDown(screen.getByTestId("graph-search"), { key: "Enter" });
    expect(mockCy.selectedCalls).toContain("entity:game:e1");
  });
});
