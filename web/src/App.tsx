import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense } from "react";

import { api, type OverviewView } from "./api";
import { RunStatusBadge } from "./components/RunButton";
import { Leads } from "./pages/Leads";
import { Live } from "./pages/Live";
import { Settings } from "./pages/Settings";
import { Streamer } from "./pages/Streamer";
import { ViewerTree } from "./pages/ViewerTree";
import { Viewers } from "./pages/Viewers";
import { matchRoute, useHashPath, type PageId } from "./router";

/** R4-F1：图谱页（cytoscape 是重组件）拆 chunk——生产构建出独立分包，首屏不背图。 */
const GraphPage = lazy(() =>
  import("./pages/GraphPage").then((module) => ({ default: module.GraphPage })),
);

/**
 * footer synthetic 徽标的析取口径（W2/r5-F3，R3#6 单源）：collection/ai/situation
 * 任一 synthetic_demo=true → 亮；三分段全缺席/全 false → 不亮（合成标示宁可缺席，
 * 不许凭失败/缺省臆造）。注：Streamer/Situ 内的同源口径是他单辖区，保留不动。
 */
export function isSyntheticRun(overview: OverviewView | undefined): boolean {
  return (
    overview?.collection?.synthetic_demo === true ||
    overview?.ai?.synthetic_demo === true ||
    overview?.situation?.synthetic_demo === true
  );
}

/** 页面键 → 组件实体装配（ROUTES 数据表在 router.tsx；懒加载缝只开图谱两路）。 */
function renderPage(page: PageId, params: string[], roomId: string) {
  switch (page) {
    case "streamer":
      return <Streamer roomId={roomId} />;
    case "viewers":
      return <Viewers roomId={roomId} />;
    case "viewer-tree":
      return <ViewerTree roomId={roomId} vid={params[0]} />;
    case "viewer-graph":
      return (
        <Suspense fallback={<div className="empty">载入图谱…</div>}>
          <GraphPage roomId={roomId} vid={params[0]} />
        </Suspense>
      );
    case "graph":
      return (
        <Suspense fallback={<div className="empty">载入图谱…</div>}>
          <GraphPage roomId={roomId} />
        </Suspense>
      );
    case "live":
      return <Live roomId={roomId} />;
    case "leads":
      return <Leads roomId={roomId} />;
    case "settings":
      return <Settings />;
  }
}

export default function App() {
  const segments = useHashPath();
  // 现布局单房间：任何数据页先解析第一条 room（uid 由 config 房号背书）。
  const rooms = useQuery({ queryKey: ["rooms"], queryFn: api.rooms });
  const roomId = rooms.data?.[0]?.id;
  // W2/r5-F3：synthetic_demo 全局明示（README「页面以徽标明示」承诺兑现）。
  // overview 失败静默缺省——合成标示宁可缺席，不许凭失败臆造。
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId!),
    enabled: roomId !== undefined,
  });
  const syntheticDemo = isSyntheticRun(overview.data);
  // hero 房间名：streamer.name 优先（房间 = 主播），缺档回落项目名。
  const streamerName = overview.data?.streamer?.name;
  const route = matchRoute(segments);

  let page;
  if (rooms.isError) {
    page = (
      <div className="notice">
        服务连接失败：{rooms.error instanceof Error ? rooms.error.message : String(rooms.error)}
      </div>
    );
  } else if (rooms.isLoading) {
    page = <div className="empty">正在连接 live-server…</div>;
  } else if (!roomId) {
    // ag4-F7：查询成功但空数组不得悬挂「正在连接」——显式空态。
    page = <div className="notice">服务端未配置任何房间（/api/rooms 返回空）。</div>;
  } else if (route === null) {
    page = (
      <div className="notice">
        未知路径。
        <a href="#/" style={{ marginLeft: 8 }}>
          回到主播介绍页
        </a>
      </div>
    );
  } else {
    page = renderPage(route.page, route.params, roomId);
  }

  return (
    <>
      <header className="hero">
        <div className="container">
          {/* 英雄区上行 = 应用品牌（用户裁决摆位：红箭头行 = 虚梦应用标题，
              黄箭头行 = 房间名=主播名 + ＋房间入口）。 */}
          <h1>虚梦 · Vtuber Dream</h1>
          {/* Z3 定稿序：主播介绍（首页）→ 舰长列表 → 直播数据 → 线索账本（末位）→ 图谱 → 设置。 */}
          <nav className="nav">
            <a href="#/">主播介绍</a>
            <a href="#/viewers">舰长列表</a>
            <a href="#/live">直播数据</a>
            <a href="#/leads">线索账本</a>
            <a href="#/graph">图谱</a>
            <a href="#/settings">设置</a>
          </nav>
        </div>
        {/* Z4c：hero 去触发钮——动作全数落页（哪个页面数据由哪个动作产出，钮住哪），
            hero 只保留只读状态徽标（RunTracker 全局共享，任何页面触发的 run 在此回报）。
            房间模型：当前房间 = 主播名（房间即主播），名列最前，点击回主播介绍页；
            「＋房间」行为 = 引导（跳设置页），多房间后端不在本原型。 */}
        <div className="container hero-runbar">
          <a
            className="room-current"
            href="#/"
            title="当前房间（房间 = 主播）"
            data-testid="room-current"
          >
            {typeof streamerName === "string" && streamerName
              ? streamerName
              : (rooms.data?.[0]?.project_name ?? "直播房间")}
          </a>
          <RunStatusBadge />
          <a
            className="room-entry"
            href="#/settings"
            title="房间配置在设置页——单房间原型，入口负责引导"
          >
            ＋ 房间
          </a>
        </div>
      </header>
      <main className="container">{page}</main>
      <footer className="container footer">
        公开信息感知原型 · 平台事实、AI语义、状态判断和行动建议分层展示
        {syntheticDemo && (
          <span className="badge" data-testid="app-synthetic">
            synthetic_demo 合成演示数据
          </span>
        )}
      </footer>
    </>
  );
}
