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

live-audience serve [-c config.yaml] [--port 3781]  # 绑定 127.0.0.1（无鉴权 = 仅本机）
```

无账号离线预览面板：把 `tests-fixtures/demo/` 的内容铺进 config 的 `output_dir` 后 `serve` 即可
（合成布景母机 `live-core/src/demo.rs` 仅供测试/走查，产品不再内置演示运行时）。

## HTTP/CLI 面

- 数据端点：`GET /api/rooms`、`/rooms/{uid}/overview|viewers|graph`、`/rooms/{uid}/viewers/{vid}/tree|graph`、`GET /api/config`（cookie/api_key 只回存在性布尔，永不回显原文；写面已删——改配置 = 编辑 config.yaml 重启）、`GET /api/budget`（预算闸现值 + 主钮旁预估：名册/新鲜人数/预估 CNY/ETD——新鲜口径与运行内预算闸同源）、`POST /rooms/{uid}/leads/{lead_id}/approve|reject`（线索审批/拒绝缝，幂等重放）。
- 触发通道：`POST /api/runs {kind, force?, viewer_uid?}` → `202 {run_id}`（预算闸：配置 `ai.run_budget_cny` 后，预估严格大于预算即阻断——run 落 failed + outcome.budget_block 四数，零 LLM 请求落地）；轮询 `GET /api/runs/{id}` 看状态机 `queued → collecting → episodes → per_viewer_ai → audience → done | failed(partial, budget_block)` 与最近 50 条 events。
  - kind 六值（动作平面）：`full`（采集+AI 连环）、`viewer`（单舰长，须 viewer_uid）、`collect_streamer` / `collect_guards`（事实层采集，跑完采集即终局）、`ai_viewers`（逐舰长 AI，写盘后即终局，不跑整体态势）/ `ai_audience`（整体态势聚合，幂等缓存自动复用已完成舰长感知）。
  - spend_mode 已删除（删码收口）：省钱语义由缓存短路默认正确化——扇出名册全员但行内 `complete_cache(input_hash)` 短路，未变者零 LLM；预估与执行同源（fresh = 输入哈希已变 ∪ 无完整旧结论），不存在「名册上限估计误杀实际零花费 run」的负循环。旧请求面携 spend_mode 键一律 422 讲规则。
  - 校验失败 `422`（kind=viewer 必须有 viewer_uid、与 force 互斥；kind=full 不接受 viewer_uid；四个分层 kind 一律拒绝 viewer_uid 与 force；非布尔 force / 超长 uid 同拒）。
  - 已有未终态 run → `409`，错文携带在飞 run_id（同一时刻只允许一个真实 run）。
  - 请求体超 64KiB 等抽取层拒绝 → 状态码保留 + `{error}` JSON 信封（含 413）。
- 重采保 AI：`ai/`（认知层缓存）与 `graph/ history/`（长期记忆）三面均永不被采集推倒——采集只重建事实面（viewers/site/shared + 顶层 JSON）；事实面快照仍照旧归档进 `history/snapshots`。旧 AI 结论保留作参考：舰长行带**时效位** `ai_stale`（true=该舰长信源已更新·待重判，哈希翻面即亮、重判即灭；false=时效内；null=无参考旧结论）。失效唯凭 per-舰长 `input_hash`——哈希件刻意摘除观察时刻类过程时间戳（episode `observed_at`、summary 请求数/耗时、`platform_snapshot.captured_at`）：事实相同的两次采集必同哈希，重采 + 重跑 AI 的成本下限 = 零。
- 面板（`web/`）：hero = 应用品牌 + 主导航（主播介绍/舰长列表/直播数据/线索账本）+ 当前房间 pill（房间=主播，单房间原型）+ 只读 run 状态徽标（含 partial 标记与 events 流）；**动作不落 hero，落页面**（钮随身段——哪个页面消费哪个产物，钮就住哪个页面）：
  - 主播介绍页（首屏）：制片人复盘卡居首卡（上一场次复盘，只读 fact 层）；主播卡（头像/签名/平台事实徽标）；**制片人简报卡**（AI 推断层）：situation 终局 `front_brief` 句句带出处（refs chip 可点跳归属观众个人树），三态 = 未生成空缺位（含一键重跑）/ 沉默渠（「本轮证据不足以成简报」= AI 宁缺毋滥的有效结论）/ 就绪（带覆盖时段与生成时间戳），任一舰长信源变则亮 stale 徽标；感知动作区 = 「触发全量感知」唯一主钮（敏感谨慎钮：inline 两段确认，直陈花费上界/409 互斥）+ 钮旁 `预估 ≈¥…（新鲜 n/m 人）· 约 N~M 分钟` 行（名册缺/空落「预估 —」）+「分层跑」次级菜单（主播 AI 分析/三种采集面动作的各页引导）。
  - 舰长列表页 = 舰长采集＋舰长 AI 分析（空池另附单查引导——单查不清场：只覆写目标一人，其余舰长事实/site/shared 原样）+「信源已更新·待重判」时效徽标（列表行/舰长态势页头/主播页舰长条三处同源）+ 关系列四微件（第几次来/距上次 N 天/身份一句（AI 徽标）/最新动态——缺件一律落「未知」，不编数字不补文案）。
  - 舰长态势页 = Episode 时间线（每行恒落「发布于（平台行为时刻）/采集于（我们看到这条的时刻）」语义徽标其一）+ mention 定位高亮。
  - 直播数据页 = 主播采集（本页数据源；`shared/live_records.json` 场次档案：最后一场 vs 上周均值对比 + 全场次表，记录空则显式空态）。
  - 线索账本末位：pending 按持有人分组折叠展示，行级「批准/拒绝」一击即飞（拒绝可携拒因 chip 白名单＋≤80 字注记，全空合法）；组级「全批/全拒」逐行 fan-out；已拒绝徽标展开回看拒因。
  - 图谱（`#/graph` 直链仍可达，不入主导航）：cytoscape 四层调色；整体图默认折叠视图只展 Viewer/Entity/状态-行动主骨架——参数 `?kinds=A,B` 自定义、`?kinds=all` 逃生门回全量；默认视图经内容寻址物化三通道 gz/br + ETag 304。
  - 「vs 上轮」delta 为底层参考信号保留在 overview payload，不上页面。

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

AI 终局提交中的 `leads[]`（经终局校验白名单：search/creator/video/room）由程序记入**图库 `discovery_leads` 表**（账本 = 表为正本；旧 `leads.jsonl` 只做一次性迁移源——含坏行时迁移守卫响铃停火、文件原地、不入半份；成功则导入归档为 `leads.jsonl.bak`，读面只走表）。身份为 `(type, locator)` 的内容哈希（dedupe_key 主键即幂等唯一键），重复线索不增行；状态机 `pending_approval → approved → consumed / rejected`（另有 `deferred` 搁置态，不入主链）。人工审批唯一通道：线索账本页的「批准/拒绝」钮（`POST /api/rooms/{room}/leads/{lead_id}/approve|reject`，`lead_id` = 行 `dedupe_key`；幂等——重放返回相同终态不写账本，已拒绝行带相异非空拒因重放 = 422 留档不可改写；不存在=404；非法迁移=422 讲规则；迁移守卫坏行=500 停火）。拒绝可携拒因：chip 白名单（太泛/不对路/已知道/做不了，唯一真源 = overview `leads.reject_chip_reasons` 下发）+ ≤80 字自由注记，全空合法（留档 NULL/NULL）。

- 消费：采集（collect）尾段按 `collection.lead_fetch_budget_per_run`（默认 0＝完全休眠）消费 `approved` 行，成功行记 `yield_count`；>0 会产生额外 B 站请求，同样受 `request_delay_seconds` 限速。
- 自治位：`collection.leads_autonomy`（0|1，默认 0=纯人工）。置 1（L1）后，collect 尾段在预算消费前先自动批准 pending 行——谓词限 creator/search 型、且 creator 目标 uid 不在本房间既有名册（viewers/*.json ∪ 主播 uid）——账本 resolution_note 记「L1 自动批准」，再照常按预算消费；线索页标题行徽标可读当前位。
- 回喂：下一轮 AI 提示面上注入一行摘要（计数 + 最新消费），以及拒绝聚合线（`[lead_reject] 上轮被拒 N 条：chip 计数 + 最近注记`；零被拒时不注入一字节），作为定向增长的上下文信号。

## 运行参数（AI 电梯与读面收口）

- `ai.agent.viewer_token_budget`（默认 200_000）：单舰长 agent 每轮 LLM 请求后累计核对 total_tokens，超限熔断终止该舰长并重试。
- `ai.agent.max_parallel_viewers`（默认 4）：并行舰长 agent 上限（Semaphore 许可）。
- `ai.agent.max_llm_rpm`（默认 0=关闭）：全 run 级 LLM 出队上限（requests/min，预约制漏桶；429/503 依 Retry-After 上浮冷却）。
- `perception.graph_default_expanded_kinds`（默认 `[Viewer, Entity, InterestState, Situation, Action]`，允许七类全集或单值 `all`）：整体图谱端点默认折叠白名单；查询参数 `?kinds=csv|all` 可覆盖。
- `perception.graph_row_limit`（默认 5000，≥1）：图读面导出行帽（与 parity 钉 `GRAPH_QUERY_LIMIT=500` 两条独立闸线，勿混）。
- `ai.run_budget_cny`（默认空 = 不设闸）：单次 run 人民币预算闸——预估（每舰长 50 万 input + 6.25 万 output token ¥2/¥8 每百万 + audience 同额平段；人数口径 = fresh 而非名册上限）严格大于闸即阻断（failed + outcome.budget_block 四数），阻断零 LLM 请求。实耗真相唯一源 = `{output}/ai/state.json` 的 `usage` 键；月度对账请用平台计费后台（本地不维护第二账源）。

## 仓库边界

代码 + README/CONTRIBUTING + `tests-fixtures/`（黄金样本）在此仓；设计规格、评审档案、走查证据与私人文档在私有仓库维护。不要把私仓档案带进本仓提交。
