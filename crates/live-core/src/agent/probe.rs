//! agent-check 探针工具组（移植 Python `runtime.py:234-291`）。
//!
//! 用途双栖：wiremock 剧本测试（协议 fixtures 的标准载具）+ env-gated 真实
//! DeepSeek agent-check（M3-D）。拒绝语与 Python 逐字一致。

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::runtime::{
    AgentRuntime, AgentRuntimeError, AgentSpec, AgentTool, AttemptPlan, RunCtx, SubmissionSlot,
    TerminalOutcome, Trace, make_terminal_tool, run_toolcall_agent,
};
use crate::config::Config;
use crate::models::ProbeResult;

/// 探针校验错误语（Python 逐字）。
pub const PROBE_VALIDATION_ERROR: &str = "a must be 7, b must be 14, total must be 21";

pub struct ProbeContext {
    pub values: BTreeMap<String, i64>,
    pub submission: Option<ProbeResult>,
    pub slot: SubmissionSlot,
}

impl ProbeContext {
    pub fn new(seed: i64) -> Self {
        let mut values = BTreeMap::new();
        values.insert("seed".to_string(), seed);
        Self {
            values,
            submission: None,
            slot: SubmissionSlot::default(),
        }
    }
}

impl RunCtx for ProbeContext {
    fn slot(&mut self) -> &mut SubmissionSlot {
        &mut self.slot
    }
}

fn simple_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// Python 契约：get_probe_seed / multiply_probe_seed / submit_probe_result。
/// `strict_mode=false` 的语义在 Rust 侧 = 我方 validators 全权（同一裁决链）。
pub fn probe_tools() -> Vec<AgentTool<ProbeContext>> {
    vec![
        AgentTool {
            name: "get_probe_seed".to_string(),
            description: "返回验收测试的起始数字。".to_string(),
            parameters: simple_schema(json!({}), &[]),
            terminal: false,
            handler: Box::new(|ctx: &mut ProbeContext, _args| {
                json!(ctx.values.get("seed").copied().unwrap_or(0))
            }),
        },
        AgentTool {
            name: "multiply_probe_seed".to_string(),
            description: "把上一步工具返回的数字乘以指定倍数。".to_string(),
            parameters: simple_schema(
                json!({
                    "seed": {"type": "integer"},
                    "factor": {"type": "integer"},
                }),
                &["seed", "factor"],
            ),
            terminal: false,
            handler: Box::new(|ctx: &mut ProbeContext, args| {
                let seed = args.get("seed").and_then(Value::as_i64).unwrap_or(0);
                let factor = args.get("factor").and_then(Value::as_i64).unwrap_or(0);
                let value = seed * factor;
                ctx.values.insert("multiplied".to_string(), value);
                json!(value)
            }),
        },
        make_terminal_tool(
            "submit_probe_result",
            "提交验收结果；这是探针唯一有效终局。",
            |ctx: &mut ProbeContext, submission: &ProbeResult| {
                if submission.a == 7 && submission.b == 14 && submission.total == 21 {
                    ctx.submission = Some(submission.clone());
                    TerminalOutcome::Accept(json!({"total": submission.total}))
                } else {
                    TerminalOutcome::Reject(vec![PROBE_VALIDATION_ERROR.to_string()])
                }
            },
        ),
    ]
}

/// agent-check 的 Agent 组装（Python run_agent_check_async 的 instruction 文案逐字）。
pub fn probe_spec() -> AgentSpec<ProbeContext> {
    AgentSpec {
        name: "Reasoning Multi Tool Call Probe".to_string(),
        instructions: "严格执行：调用get_probe_seed；读取结果后调用multiply_probe_seed，factor=2；\
                       最后调用submit_probe_result提交a=7,b=14,total=21。普通文本不是有效输出。"
            .to_string(),
        tools: probe_tools(),
    }
}

/// 探针必须走通的工具调用顺序（Python runtime.py:311 expected 序列）。
const EXPECTED_TOOL_SEQUENCE: [&str; 3] = [
    "get_probe_seed",
    "multiply_probe_seed",
    "submit_probe_result",
];

/// Python `run_agent_check_async`（runtime.py:276）逐键 parity：真实端点验收——
/// 探针定向工具序列 + 终局 Tool Call 值校验（a=7/b=14/total=21）+ 结果摘要。
/// 调用面只许 env-gated 入口（live-audience agent-check 需 VTD_AGENT_CHECK=1，
/// AGENTS.md 质量门禁·真实端点 opt-in）。
pub async fn run_agent_check_async(config: &Config) -> Result<Value, AgentRuntimeError> {
    let runtime = AgentRuntime::from_ai_config(&config.ai)?;
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let trace_path = config
        .ai
        .agent
        .local_trace
        .then(|| config.output_dir.join("ai/traces/agent-check.jsonl"));
    let mut trace = Trace::new(trace_path);
    let outcome = run_toolcall_agent::<ProbeContext, ProbeResult>(
        &runtime,
        &mut spec,
        AttemptPlan {
            label: "agent-check",
            prompt: "开始思考模式多轮Tool Call与终局Tool Call验收。",
            max_turns: 8.max(config.ai.agent.max_turns) as usize,
            retries: config.ai.agent.run_retries.max(0) as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
        },
        &mut ctx,
        &mut trace,
    )
    .await?;
    let stats = &trace.stats;
    if stats.tool_names.len() < EXPECTED_TOOL_SEQUENCE.len()
        || stats.tool_names[..EXPECTED_TOOL_SEQUENCE.len()] != EXPECTED_TOOL_SEQUENCE
    {
        return Err(AgentRuntimeError::Protocol(format!(
            "unexpected tool sequence: {:?}",
            stats.tool_names
        )));
    }
    Ok(serde_json::json!({
        "status": "PASS",
        "api": config.ai.api,
        "model": config.ai.model,
        "reasoning_enabled": config.ai.reasoning.enabled,
        "reasoning_replay": config.ai.reasoning.replay_content,
        "output_protocol": "tool_call_only",
        "terminal_tool": "submit_probe_result",
        "ordinary_text_final": false,
        "llm_calls": stats.llm_calls,
        "tool_calls": stats.tool_calls,
        "tool_sequence": stats.tool_names,
        "output": outcome.submission,
    }))
}

/// Python `run_agent_check`（runtime.py:332）的同步壳 = `asyncio.run(...)` parity。
/// CLI（同步 main）只调本函数；tokio runtime 是内部细节，不外泄。
pub fn run_agent_check(config: &Config) -> Result<Value, AgentRuntimeError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| AgentRuntimeError::Config(format!("tokio runtime: {err}")))?;
    runtime.block_on(run_agent_check_async(config))
}
