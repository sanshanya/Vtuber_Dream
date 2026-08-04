# CONTRIBUTING — 开发门禁与纪律

## 门禁（合并前必须全绿，字面与 CI 一字一致）

Rust 三件套（仓库根）：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Web 三件套（`web/` 目录）：

```bash
npm ci
npx tsc --noEmit
npx vitest run
npm run build
```

请在本地先跑通再提交；`.github/workflows/ci.yaml` 用的就是上方字面。
请注意：任何「管道吞掉 exit code」的拼法（`cargo test | grep …`）都不叫过门禁——
以 exit code 为准（`echo $?`）。

## 真实端点纪律（显式 opt-in）

- 全部单元/集成测试**离线可跑**（wiremock in-process）。真实 B 站/LLM 出网
  只属于显式 opt-in 路径：
- `VTD_AGENT_CHECK=1 live-audience agent-check -c config.yaml`
  ——不设 env 连 config 都不读。
- 真实采集/single-run 验收走开发者本机 config（cookie/api_key 从环境注入，
  **绝不入任何提交、日志、Demo 或构建产物**）。
- fixture 允许的最高形式：`SESSDATA=test`、`api_key: test` 字面量。

## 代码纪律速查（同 AGENTS.md 哲学核心）

- AI-first 但非 AI-only：程序管事实/身份/时间/校验，AI 只产结构化终局 Tool Call。
- 事实/推断/状态/行动四层分离——badge 调色、DTO、graph source_kind 同语言。
- 魔数命名 + 钉（默认值、用途、测试三件套）。
- 小步提交；修复 Bug 必须先有能复现的测试。
- 模块边界以 AGENTS.md §5 表为准；单文件 >500 行需理由，>800 行应拆。

## 文档边界

本仓只含 **代码 + README + tests-fixtures**。设计/评审/kickoff 档案、
GUI 证据树、CONFIG/CONTEXT 文档一律留在私仓——不要把它们带进本仓提交。
