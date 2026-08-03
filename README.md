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
