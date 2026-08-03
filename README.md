# Vtuber Dream

以 B 站公开事实为输入、以 AI Agent 为认知核心、以时序图谱为长期记忆、以主播内容决策为输出的观众态势感知工具。

**现状**：Rust 重写进行时（Phase 1 / G1）。本仓库仅公开代码与测试样本；设计规格、ADR 与术语表在私有仓库维护，不公开发布。

前身（Python 参考实现 + 黄金样本 fixture 来源）：`live-audience-mvp`（私有仓库，本项目 tests-fixtures 由其 dump 产出）。

## 合规边界

本工具仅处理 B 站**公开**数据；产出仅限主播本人本地使用，不公开传播；Cookie/API Key 永不入库。

服务默认仅绑定 `127.0.0.1`（无鉴权 = 仅本机）。**观察账号（Cookie 所属）存在被 B 站风控限制的现实风险**，建议使用专用低价值账号，使用者自担账号后果。

## 开发

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 线索账本（M4.x 薄切）

AI 终局提交中的 `leads[]`（经终局校验白名单：search/creator/video/room）由程序记入 `output_dir/leads.jsonl`：身份为 `(type, locator)` 的内容哈希，重复线索不增行；状态机 `pending_approval → approved → consumed / rejected`。**人工审批 = 直接编辑账本行的 status 字段**（无 UI、无自动抓取）——这是薄切设计有意为之。

- 消费：采集（collect）尾段按 `collection.lead_fetch_budget_per_run`（默认 0＝完全休眠）消费 `approved` 行，成功行记 `yield_count`；>0 会产生额外 B 站请求，同样受 `request_delay_seconds` 限速。
- 回喂：下一轮 AI 提示面上注入一行摘要（计数 + 最新消费），作为定向增长的上下文信号。
