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
- 触发通道：`POST /api/runs {kind:"full"|"viewer", force?, viewer_uid?}` → `202 {run_id}`；轮询 `GET /api/runs/{id}` 看状态机 `queued → collecting → episodes → per_viewer_ai → audience → done | failed(partial)` 与最近 50 条 events。
  - 校验失败 `422`（kind=viewer 必须有 viewer_uid、与 force 互斥；kind=full 不接受 viewer_uid；非布尔 force / 超长 uid 同拒）。
  - 已有未终态 run → `409`（同一时刻只允许一个真实 run）。
  - 请求体超 64KiB 等抽取层拒绝 → 状态码保留 + `{error}` JSON 信封（含 413）。
  - demo 模式下 `POST /api/runs` 返回静态合成快照（幂等：重复触发返回同一 run_id）。
- 面板（`web/`）：hero 常驻触发钮 + run 状态徽标（含 partial 标记与 events 流）；Dashboard（投入/产出/花费上限估算/vs 上轮 delta/整体态势/线索账本）、Viewers（空池单查引导）、个人树（Episode 时间线 + mention 定位高亮）、图谱（cytoscape 四层调色）、设置（白名单写入回显）。

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
