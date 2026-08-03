//! agent 层（design §4）：runtime → tools → validators → prompts → pipeline。
//! M3-A 挂载：runtime（BYO chat 类型 + 终局协议循环 + trace）+ probe（agent-check 工具组）。
//! M3-B 挂载：tools（调查工具 + ResearchService）。
//! M3-C 挂载：validators（终局校验台 +§9.1 修复 1/6/8 + leads）。

pub mod probe;
pub mod prompts;
pub mod runtime;
pub mod tools;
pub mod validators;
