//! agent 层（design §4）：runtime → tools → validators → prompts → pipeline。
//! M3-A 挂载：runtime（BYO chat 类型 + 终局协议循环 + trace）+ probe（agent-check 工具组）。
//! M3-B 挂载：tools（调查工具 + ResearchService）。
//! M3-C 挂载：validators（终局校验台 +§9.1 修复 1/6/8 + leads）。
//! M4-A 挂载：pipeline（输入小件：hash/usage/bundle/audience 索引）。
//! Z3/P0-4 挂载：throttle（限速漏桶 leaky bucket——全局 LLM 请求节律门）。

pub mod history;
pub mod pipeline;
pub mod probe;
pub mod prompts;
pub mod redact;
pub mod runtime;
pub mod specs;
pub mod throttle;
pub mod tools;
pub mod validators;
