import { useQuery } from "@tanstack/react-query";

import { api } from "./api";
import { RunButton } from "./components/RunButton";
import { Dashboard } from "./pages/Dashboard";
import { GraphPage } from "./pages/GraphPage";
import { Settings } from "./pages/Settings";
import { ViewerTree } from "./pages/ViewerTree";
import { Viewers } from "./pages/Viewers";
import { useHashPath } from "./router";

export default function App() {
  const segments = useHashPath();
  // 现布局单房间：任何数据页先解析第一条 room（uid 由 config 房号背书）。
  const rooms = useQuery({ queryKey: ["rooms"], queryFn: api.rooms });
  const roomId = rooms.data?.[0]?.id;

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
  } else if (segments.length === 0) {
    page = <Dashboard roomId={roomId} />;
  } else if (segments[0] === "viewers" && segments.length === 1) {
    page = <Viewers roomId={roomId} />;
  } else if (segments[0] === "viewers" && segments.length === 3 && segments[2] === "tree") {
    page = <ViewerTree roomId={roomId} vid={segments[1]} />;
  } else if (segments[0] === "viewers" && segments.length === 3 && segments[2] === "graph") {
    page = <GraphPage roomId={roomId} vid={segments[1]} />;
  } else if (segments[0] === "graph" && segments.length === 1) {
    page = <GraphPage roomId={roomId} />;
  } else if (segments[0] === "settings" && segments.length === 1) {
    page = <Settings />;
  } else {
    page = (
      <div className="notice">
        未知路径。
        <a href="#/" style={{ marginLeft: 8 }}>
          回到房间面板
        </a>
      </div>
    );
  }

  return (
    <>
      <header className="hero">
        <div className="container">
          <h1>{rooms.data?.[0]?.project_name ?? "live-audience"}</h1>
          <nav className="nav">
            <a href="#/">房间面板</a>
            <a href="#/viewers">观众列表</a>
            <a href="#/graph">图谱</a>
            <a href="#/settings">设置</a>
          </nav>
        </div>
        {/* design §10「全部页头触发钮」裁决：hero 单点挂载；追踪态由 RunTracker 全局共享。 */}
        <div className="container hero-runbar">
          <RunButton />
        </div>
      </header>
      <main className="container">{page}</main>
      <footer className="container footer">
        公开信息感知原型 · 平台事实、AI语义、状态判断和行动建议分层展示
      </footer>
    </>
  );
}
