import { useQuery } from "@tanstack/react-query";

import { api } from "./api";
import { RunButton } from "./components/RunButton";
import { GraphPage } from "./pages/GraphPage";
import { Leads } from "./pages/Leads";
import { Live } from "./pages/Live";
import { Settings } from "./pages/Settings";
import { Streamer } from "./pages/Streamer";
import { ViewerTree } from "./pages/ViewerTree";
import { Viewers } from "./pages/Viewers";
import { useHashPath } from "./router";

export default function App() {
  const segments = useHashPath();
  // 现布局单房间：任何数据页先解析第一条 room（uid 由 config 房号背书）。
  const rooms = useQuery({ queryKey: ["rooms"], queryFn: api.rooms });
  const roomId = rooms.data?.[0]?.id;
  // W2/r5-F3：synthetic_demo 全局明示（README「页面以徽标明示」承诺兑现）。
  // 数据面沿用 Streamer 的 collection/ai/situation 任一析取口径；overview 失败
  // 静默缺省——合成标示宁可缺席，不许凭失败臆造。
  const overview = useQuery({
    queryKey: ["overview", roomId],
    queryFn: () => api.overview(roomId!),
    enabled: roomId !== undefined,
  });
  const overviewData = overview.data ?? {};
  const syntheticDemo =
    overviewData.collection?.synthetic_demo === true ||
    overviewData.ai?.synthetic_demo === true ||
    overviewData.situation?.synthetic_demo === true;

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
    page = <Streamer roomId={roomId} />;
  } else if (segments[0] === "viewers" && segments.length === 1) {
    page = <Viewers roomId={roomId} />;
  } else if (segments[0] === "viewers" && segments.length === 3 && segments[2] === "tree") {
    page = <ViewerTree roomId={roomId} vid={segments[1]} />;
  } else if (segments[0] === "viewers" && segments.length === 3 && segments[2] === "graph") {
    page = <GraphPage roomId={roomId} vid={segments[1]} />;
  } else if (segments[0] === "live" && segments.length === 1) {
    page = <Live roomId={roomId} />;
  } else if (segments[0] === "leads" && segments.length === 1) {
    page = <Leads roomId={roomId} />;
  } else if (segments[0] === "graph" && segments.length === 1) {
    page = <GraphPage roomId={roomId} />;
  } else if (segments[0] === "settings" && segments.length === 1) {
    page = <Settings />;
  } else {
    page = (
      <div className="notice">
        未知路径。
        <a href="#/" style={{ marginLeft: 8 }}>
          回到主播介绍页
        </a>
      </div>
    );
  }

  return (
    <>
      <header className="hero">
        <div className="container">
          {/* 英雄区名字 = 主播本人；副徽标 = 产品中文薄名「虚梦」（不是 config 工程代号——
              项目名是内部记号，产品名是给人看的。主播缺档时名字栏落产品名。 */}
          <h1>
            {typeof overviewData.streamer?.name === "string" && overviewData.streamer.name
              ? overviewData.streamer.name
              : "虚梦"}
            <span className="badge hero-project">虚梦 · Vtuber Dream</span>
          </h1>
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
        {/* design §10「全部页头触发钮」裁决：hero 单点挂载；追踪态由 RunTracker 全局共享。
            Z5 房间入口钮：行为 = 引导（跳「设置」页的房间配置区），不做真实多房间后端。 */}
        <div className="container hero-runbar">
          <RunButton />
          <a className="room-entry" href="#/settings" title="房间配置在设置页——单房间原型，入口负责引导">
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
