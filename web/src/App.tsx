import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense } from "react";

import { api, type OverviewView } from "./api";
import { RunStatusBadge } from "./components/RunButton";
import { matchRoute, useHashPath, type PageId } from "./router";

/** 八页全懒加载——每路由一个 chunk，首屏 main 只背壳
 *  （react + react-query + App + RunTracker + styles）。图谱页（cytoscape 重组件）
 *  之外的七页同样是路由级边界，没有提前融合进首屏包的理由。 */
const Streamer = lazy(() =>
  import("./pages/Streamer").then((module) => ({ default: module.Streamer })),
);
const Viewers = lazy(() =>
  import("./pages/Viewers").then((module) => ({ default: module.Viewers })),
);
const ViewerTree = lazy(() =>
  import("./pages/ViewerTree").then((module) => ({ default: module.ViewerTree })),
);
const Live = lazy(() => import("./pages/Live").then((module) => ({ default: module.Live })));
const Leads = lazy(() => import("./pages/Leads").then((module) => ({ default: module.Leads })));
const GraphPage = lazy(() =>
  import("./pages/GraphPage").then((module) => ({ default: module.GraphPage })),
);

/** 页面键 → 组件实体装配（ROUTES 数据表在 router.tsx；七页全懒，
 *  Suspense 边界唯一，挂在 <main> 出口——见 App 返回体）。 */
function renderPage(page: PageId, params: string[], roomId: string) {
  switch (page) {
    case "streamer":
      return <Streamer roomId={roomId} />;
    case "viewers":
      return <Viewers roomId={roomId} />;
    case "viewer-tree":
      return <ViewerTree roomId={roomId} vid={params[0]} />;
    case "viewer-graph":
      return <GraphPage roomId={roomId} vid={params[0]} />;
    case "graph":
      return <GraphPage roomId={roomId} />;
    case "live":
      return <Live roomId={roomId} />;
    case "leads":
      return <Leads roomId={roomId} />;
  }
}

export default function App() {
  const segments = useHashPath();
  // 现布局单房间：任何数据页先解析第一条 room（uid 由 config 房号背书）。
  const rooms = useQuery({ queryKey: ["rooms"], queryFn: api.rooms });
  const roomId = rooms.data?.[0]?.id;
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId!),
    enabled: roomId !== undefined,
  });
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
    // 查询成功但空数组不得悬挂「正在连接」——显式空态。
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
              黄箭头行 = 房间名=主播名 + 只读 run 状态徽标）。 */}
          <h1>虚梦 · Vtuber Dream</h1>
          {/* 定稿序 + 裁决：主播介绍（首页）→ 舰长列表 → 直播数据 →
              线索账本 → 设置。图谱退出主导航（#/graph 原路由仍可达，
              仅不再陈列入口）。 */}
          <nav className="nav">
            <a href="#/">主播介绍</a>
            <a href="#/viewers">舰长列表</a>
            <a href="#/live">直播数据</a>
            <a href="#/leads">线索账本</a>
          </nav>
        </div>
        {/* 英雄区 hero 去触发钮——动作全数落页（哪个页面数据由哪个动作产出，钮住哪），
            hero 只保留只读状态徽标（RunTracker 全局共享，任何页面触发的 run 在此回报）。
            房间模型：单房间原型——当前房间 = 主播名（房间即主播），名列最前，点击回
            主播介绍页。 */}
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
        </div>
      </header>
      <main className="container">
        {/* 唯一懒加载边界——任何路由切页 fallback 一律走空态文案（T1 三态前的中性态）。 */}
        <Suspense fallback={<div className="state-loading">载入页面…</div>}>{page}</Suspense>
      </main>
      <footer className="container footer">
        公开信息感知原型 · 平台事实、AI语义、状态判断和行动建议分层展示
      </footer>
    </>
  );
}
