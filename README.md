# Vtuber Dream

以 B 站公开事实为输入、以 AI Agent 为认知核心、以时序图谱为长期记忆、以主播内容决策为输出的观众态势感知工具。

**现状**：Rust 实现为唯一运行时，全链已通——采集 → Episode/Mention/Entity → 单人 Perception → 整体态势 → 图/报告/serve + React 面板。本仓库仅公开代码与测试样本；设计规格、ADR 与术语表在私有仓库维护，不公开发布。前身 Python 参考实现已冻结归档（仅作黄金样本 fixture 来源，不参与任何运行路径）。

## 合规边界

本工具仅处理 B 站**公开**数据；产出仅限主播本人本地使用，不公开传播；Cookie/API Key 永不入库。

服务默认仅绑定 `127.0.0.1`（无鉴权 = 仅本机）。**观察账号（Cookie 所属）存在被 B 站风控限制的现实风险**，建议使用专用低价值账号，使用者自担账号后果。

## 快速开始

```bash
# 前端一次性构建（产物 web/dist，未构建时 / 显示构建指引而非静默 404）
cd web && npm ci && npm run build && cd ..

live-audience serve [-c config.yaml] [--port 3781]       # 绑定 127.0.0.1（无鉴权 = 仅本机）
live-audience run --demo [-c config.yaml] [--port 3781]  # 构建合成 Demo 后以其产物起服
live-audience demo [-c config.yaml] [--output dir]       # 只构建合成 Demo 数据
```

合成 Demo 数据含 `synthetic_demo: true` 标记，页面以徽标明示、与真实采集产出永不相混。

## HTTP/CLI 面

- 数据端点：`GET /api/rooms`、`/rooms/{uid}/overview|viewers|graph`、`/rooms/{uid}/viewers/{vid}/tree|graph`、`GET/PUT /api/config`（cookie/api_key 只回存在性布尔，永不回显原文；PUT 走白名单 4 键 + 显式字符串类型 + 原子写盘，非法一律 422 原文件不动）。
- 触发通道：`POST /api/runs {kind, force?, viewer_uid?}` → `202 {run_id}`；轮询 `GET /api/runs/{id}` 看状态机 `queued → collecting → episodes → per_viewer_ai → audience → done | failed(partial)` 与最近 50 条 events。
  - kind 六值（Z4 动作平面）：`full`（采集+AI 连环）、`viewer`（单舰长，须 viewer_uid）、`collect_streamer` / `collect_guards`（事实层采集，跑完采集即终局）、`ai_viewers`（逐舰长 AI，写盘后即终局，不跑整体态势）/ `ai_audience`（整体态势聚合，幂等缓存自动复用已完成舰长感知）。
  - 校验失败 `422`（kind=viewer 必须有 viewer_uid、与 force 互斥；kind=full 不接受 viewer_uid；四个分层 kind 一律拒绝 viewer_uid 与 force；非布尔 force / 超长 uid 同拒）。
  - 已有未终态 run → `409`，错文携带在飞 run_id（同一时刻只允许一个真实 run）。
  - 请求体超 64KiB 等抽取层拒绝 → 状态码保留 + `{error}` JSON 信封（含 413）。
  - demo 模式下 `POST /api/runs` 返回静态合成快照（幂等：重复触发返回同一 run_id）。
- Z5 重采保 AI：`ai/`（认知层缓存）与 `graph/ history/`（长期记忆）三面均永不被采集推倒——采集只重建事实面（viewers/site/shared + 顶层 JSON）；事实面快照仍照旧归档进 `history/snapshots`。旧 AI 结论保留作参考：舰长行带**时效位** `ai_stale`（true=该舰长信源已更新·待重判，哈希翻面即亮、重判即灭；false=时效内；null=无参考旧结论）。失效唯凭 per-舰长 `input_hash`——哈希件刻意摘除观察时刻类过程时间戳（episode `observed_at`、summary 请求数/耗时、`platform_snapshot.captured_at`）：事实相同的两次采集必同哈希，重采 + 重跑 AI 的成本下限 = 零。
- 面板（`web/`）：hero = 应用品牌 + 导航 + 当前房间 pill（房间=主播）+ ＋房间入口钮（引导跳设置页，单房间原型）+ 只读 run 状态徽标（含 partial 标记与 events 流）；**动作不落 hero，落页面**（Z4 动作平面：钮随身段——哪个页面消费哪个产物，钮就住哪个页面）——主播介绍页 = 全量感知（敏感谨慎钮：inline 两段确认，直陈时长/花费上界/哈希失效/409 互斥）＋主播 AI 分析；舰长列表页 = 舰长采集＋舰长 AI 分析（空池另附单查引导）+ 行面「信源已更新·待重判」时效徽标；直播数据页 = 主播采集（本页数据源）。首页主播介绍（主播卡头像/签名/平台事实徽标 + 大图数指标条 + 运行概览四层 legend + 整体态势，executive_summary 走自绘 Markdown 渲染）、舰长列表（头像 + 大航海/勋章身份列）→ 舰长态势（Episode 时间线 + mention 定位高亮）、直播数据（`shared/live_records.json` 场次档案：最后一场 vs 上周均值对比 + 全场次表，记录空则显式空态）、线索账本末位、图谱（cytoscape 四层调色）、设置（白名单写入回显）。「vs 上轮」delta 为底层参考信号保留在 overview payload，不上页面。

## 真实端点（显式 opt-in）

```bash
VTD_AGENT_CHECK=1 live-audience agent-check -c config.yaml   # 真实 DeepSeek 探针验收
```

不设环境变量时连 config 都不读。cookie/真实 key 只从本机环境注入，永不入库入档。

## 开发门禁

与 `CONTRIBUTING.md` / CI（`.github/workflows/ci.yaml`）字面一致：

```bash
# Rust（仓库根）
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Web（web/）
npm ci
npx tsc --noEmit
npx vitest run
npm run build
```

## 线索账本（M4.x 薄切）

AI 终局提交中的 `leads[]`（经终局校验白名单：search/creator/video/room）由程序记入 `output_dir/leads.jsonl`：身份为 `(type, locator)` 的内容哈希，重复线索不增行；状态机 `pending_approval → approved → consumed / rejected`（另有 `deferred` 搁置态，不入主链）。**人工审批 = 直接编辑账本行的 status 字段**（无 UI、无自动抓取）——这是薄切设计有意为之。

- 消费：采集（collect）尾段按 `collection.lead_fetch_budget_per_run`（默认 0＝完全休眠）消费 `approved` 行，成功行记 `yield_count`；>0 会产生额外 B 站请求，同样受 `request_delay_seconds` 限速。
- 回喂：下一轮 AI 提示面上注入一行摘要（计数 + 最新消费），作为定向增长的上下文信号。

## 仓库边界

代码 + README/CONTRIBUTING + `tests-fixtures/`（黄金样本）在此仓；设计规格、评审档案、走查证据与私人文档在私有仓库维护。不要把私仓档案带进本仓提交。
