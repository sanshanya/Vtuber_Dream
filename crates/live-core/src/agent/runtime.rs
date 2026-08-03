//! Agent 运行时（移植 Python `agent/runtime.py`，design §M3 协议红线）。
//!
//! 协议契约（逐条对照 Python `run_toolcall_agent`）：
//! - 唯一终局工具：`ctx.slot().value` 被接受即终止（final_output="accepted"）；
//! - 普通文本提前结束 → 保留全部历史（system/user/assistant 草稿/工具调用与结果），
//!   追述 user 段（草稿截断 [`DRAFT_TRUNCATE_CHARS`]），tools 列表收窄为仅终局 +
//!   `tool_choice` 具名强制重跑，forced max_turns 钳 [`FORCED_TURNS_MIN..=FORCED_TURNS_CAP`]；
//!   两轮仍无 accepted → attempt 失败（`NoTerminal`）计入线性退避重试；
//! - 重试：共 `retries+1` 次 attempt，线性退避 `backoff_seconds * attempt`；HTTP 层瞬时错
//!   （timeout/connect/本轮发送失败）只在 `chat()` 内层 ≤[`HTTP_EXTRA_ATTEMPTS`] 次压住；
//! - reasoning_content：BYO 消息结构体全程往返（回放上送）。`replay_reasoning=false` 时
//!   落历史前剥离；**永不写入 trace**；
//! - Trace：JSONL 元数据（time/event/agent/token 三元组/tool 名+tool_call_id/result_chars
//!   /elapsed_ms）——S0 漏埋教训：tool 名与显式耗时是必填字段，不接受 None。
//!
//! 传输 = async-openai `create_byot`：请求与响应均 BYO 类型（ADR 2026-08-04 源码级验证）。
//! 设计与 Python 的已知偏差（书面）：tools 数组在 forced 续跑中收窄为仅终局（Python 同义：
//! forced Agent 只挂终局工具），dispatch 仍按名字查表——比原文献节约一次 handler 迁移问题。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::AiConfig;

// ---------------------------------------------------------------------------
// BYO wire 类型（chat completions；形状对齐 Python agents SDK 实际出网 JSON）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OaiToolCall {
    pub id: String,
    #[serde(default = "function_type_str", rename = "type")]
    pub tool_type: String,
    pub function: OaiToolFn,
}

fn function_type_str() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OaiToolFn {
    pub name: String,
    /// JSON 文本（不是嵌套对象）——chat completions 契约。
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OaiMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OaiToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl OaiMessage {
    fn plain(role: &str, content: String) -> Self {
        Self {
            role: role.to_string(),
            content: Some(content),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn system(content: String) -> Self {
        Self::plain("system", content)
    }
    pub fn user(content: String) -> Self {
        Self::plain("user", content)
    }
    pub fn tool_result(call_id: String, payload: &Value) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(payload.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct OaiToolDef {
    #[serde(rename = "type")]
    tool_type: &'static str, // "function"
    function: OaiToolDefFn,
}

#[derive(Debug, Clone, Serialize)]
struct OaiToolDefFn {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Serialize)]
pub struct OaiChatRequest {
    model: String,
    messages: Vec<OaiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OaiToolDef>>,
    /// 具名强制：`{"type":"function","function":{"name":...}}`（Python forced agent 的 wire 形状）。
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    /// Python ModelSettings.reasoning → chat completions 的 reasoning_effort 槽位。
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    /// Python `extra_body={"thinking":{"type":"enabled"}}` 顶层展平位。
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<OaiThinking>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OaiThinking {
    #[serde(rename = "type")]
    thinking_type: &'static str, // "enabled"
}

#[derive(Debug, Default, Deserialize)]
pub struct OaiUsage {
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

#[derive(Debug, Deserialize)]
pub struct OaiChoice {
    pub message: OaiMessage,
    #[serde(default)]
    #[allow(dead_code)] // wire 文档面：剧本/真实响应都携带；协议不消费
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OaiChatResponse {
    #[serde(default)]
    pub choices: Vec<OaiChoice>,
    #[serde(default)]
    pub usage: Option<OaiUsage>,
}

// ---------------------------------------------------------------------------
// 错误、统计、trace
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AgentRuntimeError {
    #[error("chat transport: {0}")]
    Transport(String),
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("max turns {turns} exceeded cap {cap}")]
    MaxTurns { turns: usize, cap: usize },
    #[error("{label} ended without accepted {terminal}; validation_errors={errors:?}")]
    NoTerminal {
        label: String,
        terminal: String,
        errors: Vec<String>,
    },
    #[error("{label} failed after {attempts} attempts: {reason}")]
    Exhausted {
        label: String,
        attempts: usize,
        reason: String,
    },
    #[error("config: {0}")]
    Config(String),
}

#[derive(Debug, Default)]
pub struct RuntimeStats {
    pub llm_calls: u64,
    pub tool_calls: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub tool_names: Vec<String>,
}

fn utc_now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// JSONL 元数据 trace（每次 append-open 写一行，与 Python LocalTraceHooks 一致：
/// 中断后已落盘部分仍可用）。
#[derive(Default)]
pub struct Trace {
    pub stats: RuntimeStats,
    path: Option<PathBuf>,
}

impl Trace {
    pub fn new(path: Option<PathBuf>) -> Self {
        if let Some(path) = &path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, "");
        }
        Self {
            stats: RuntimeStats::default(),
            path,
        }
    }

    pub fn none() -> Self {
        Self::new(None)
    }

    pub fn write(&mut self, event: &str, data: Value) {
        let Some(path) = &self.path else { return };
        let mut payload = data;
        if let Value::Object(ref mut map) = payload {
            map.insert("time".to_string(), json!(utc_now_iso()));
            map.insert("event".to_string(), json!(event));
        }
        if let Ok(mut handle) = std::fs::OpenOptions::new().append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(handle, "{}", payload);
        }
    }
}

// ---------------------------------------------------------------------------
// Agent 规格、工具、终局槽位
// ---------------------------------------------------------------------------

pub type ToolHandler<C> = Box<dyn FnMut(&mut C, &Value) -> Value>;

pub struct AgentTool<C> {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub terminal: bool,
    pub handler: ToolHandler<C>,
}

pub struct AgentSpec<C> {
    pub name: String,
    pub instructions: String,
    pub tools: Vec<AgentTool<C>>,
}

impl<C> AgentSpec<C> {
    pub fn terminal_index(&self) -> Option<usize> {
        self.tools.iter().position(|t| t.terminal)
    }
}

/// 终局提交槽位：submission 的 JSON 镜像 + 本轮 validation_errors。
#[derive(Debug, Default, Clone)]
pub struct SubmissionSlot {
    pub value: Option<Value>,
    pub validation_errors: Vec<String>,
}

/// 运行上下文（Python `context.submission`/`context.validation_errors` 的镜像）。
pub trait RunCtx {
    fn slot(&mut self) -> &mut SubmissionSlot;
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TerminalArgs<S> {
    submission: S,
}

/// 终局工具工厂：serde schema 解析 → 业务校验 → 接受/拒绝。
/// 拒绝形状与 Python tools.py 三终局一致：
/// `{"accepted":false,"errors":[...],"instruction":"修正后重新调用本工具"}`，
/// 且 submission 槽必须保持 None（测试钉）。
pub fn make_terminal_tool<C, S, V>(name: &str, description: &str, validator: V) -> AgentTool<C>
where
    C: RunCtx,
    S: for<'de> Deserialize<'de> + Serialize + schemars::JsonSchema + 'static,
    V: Fn(&mut C, &S) -> Result<Value, Vec<String>> + 'static,
{
    let schema = schemars::schema_for!(TerminalArgs<S>);
    AgentTool {
        name: name.to_string(),
        description: description.to_string(),
        parameters: serde_json::to_value(schema).unwrap_or(Value::Null),
        terminal: true,
        handler: Box::new(move |ctx: &mut C, args: &Value| {
            match serde_json::from_value::<TerminalArgs<S>>(args.clone()) {
                Err(err) => {
                    let errors = vec![format!("submission schema 不合法: {err}")];
                    let slot = ctx.slot();
                    slot.validation_errors = errors.clone();
                    slot.value = None;
                    json!({
                        "accepted": false,
                        "errors": errors,
                        "instruction": "修正后重新调用本工具",
                    })
                }
                Ok(parsed) => match validator(ctx, &parsed.submission) {
                    Ok(payload) => {
                        // Python parity：每次调用都覆写 validation_errors；接受时为空。
                        ctx.slot().validation_errors.clear();
                        if let Ok(value) = serde_json::to_value(&parsed.submission) {
                            ctx.slot().value = Some(value);
                        }
                        let mut payload = payload;
                        if let Value::Object(ref mut map) = payload {
                            map.insert("accepted".to_string(), json!(true));
                        }
                        payload
                    }
                    Err(errors) => {
                        let slot = ctx.slot();
                        slot.validation_errors = errors.clone();
                        slot.value = None;
                        json!({
                            "accepted": false,
                            "errors": errors,
                            "instruction": "修正后重新调用本工具",
                        })
                    }
                },
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// 运行时
// ---------------------------------------------------------------------------

/// 普通文本草稿截断（Python `str(result.final_output)[:200_000]` 字符）。
pub const DRAFT_TRUNCATE_CHARS: usize = 200_000;
pub const FORCED_TURNS_MIN: usize = 4;
pub const FORCED_TURNS_CAP: usize = 16;
/// HTTP 层瞬时错误附加尝试（Python `AsyncOpenAI(max_retries=2)` 等价）。
const HTTP_EXTRA_ATTEMPTS: usize = 2;

pub fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

pub struct AgentRuntime {
    client: async_openai::Client<async_openai::config::OpenAIConfig>,
    model: String,
    max_tokens: i64,
    reasoning_effort: Option<String>,
    thinking_enabled: bool,
    /// false → 回放 assistant 消息前剥离 reasoning_content（Python should_replay 开关）。
    replay_reasoning: bool,
}

impl AgentRuntime {
    /// 生产入口：config.ai → client。chat_completions 唯一支持（design §协议配置表）。
    pub fn from_ai_config(ai: &AiConfig) -> Result<Self, AgentRuntimeError> {
        if ai.api != "chat_completions" {
            return Err(AgentRuntimeError::Config(format!(
                "仅支持 chat_completions API，收到 {}",
                ai.api
            )));
        }
        if ai.api_key.trim().is_empty() {
            return Err(AgentRuntimeError::Config("api_key 为空".to_string()));
        }
        let http = reqwest13::Client::builder()
            .timeout(Duration::from_secs_f64(ai.timeout_seconds.max(1.0)))
            .build()
            .map_err(|err| AgentRuntimeError::Transport(err.to_string()))?;
        let config = async_openai::config::OpenAIConfig::new()
            .with_api_base(&ai.base_url)
            .with_api_key(&ai.api_key);
        Ok(Self::build(
            async_openai::Client::with_config(config).with_http_client(http),
            &ai.model,
            ai.max_output_tokens,
            ai.reasoning.enabled,
            &ai.reasoning.effort,
            ai.reasoning.replay_content,
        ))
    }

    /// 测试接缝：注入 mock base_url（reasoning_effort 固定 "high"——剧本钉的常量）。
    pub fn for_test(
        base_url: &str,
        model: &str,
        max_tokens: i64,
        reasoning_enabled: bool,
        replay_reasoning: bool,
    ) -> Self {
        let config = async_openai::config::OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key("test");
        Self::build(
            async_openai::Client::with_config(config),
            model,
            max_tokens,
            reasoning_enabled,
            "high",
            replay_reasoning,
        )
    }

    fn build(
        client: async_openai::Client<async_openai::config::OpenAIConfig>,
        model: &str,
        max_tokens: i64,
        reasoning_enabled: bool,
        effort: &str,
        replay_reasoning: bool,
    ) -> Self {
        Self {
            client,
            model: model.to_string(),
            max_tokens,
            reasoning_effort: if reasoning_enabled && !effort.is_empty() {
                Some(effort.to_string())
            } else {
                None
            },
            thinking_enabled: reasoning_enabled,
            replay_reasoning,
        }
    }

    /// 单次 chat + 瞬时重试。非流式 + 网关超时：以 retry 兜住（S0 实测 0 失败）。
    pub async fn chat(
        &self,
        request: &OaiChatRequest,
    ) -> Result<OaiChatResponse, AgentRuntimeError> {
        let mut last_error: Option<async_openai::error::OpenAIError> = None;
        for attempt in 0..=HTTP_EXTRA_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs_f64(attempt as f64)).await;
            }
            match self.client.chat().create_byot(request).await {
                Ok(response) => return Ok(response),
                Err(err) => {
                    if !is_transient(&err) || attempt == HTTP_EXTRA_ATTEMPTS {
                        return Err(AgentRuntimeError::Transport(err.to_string()));
                    }
                    last_error = Some(err);
                }
            }
        }
        Err(AgentRuntimeError::Transport(
            last_error.expect("loop 至少一次").to_string(),
        ))
    }

    fn tool_defs<C>(tools: &[AgentTool<C>], visible: Option<usize>) -> Vec<OaiToolDef> {
        tools
            .iter()
            .enumerate()
            .filter(|(index, _)| visible.is_none_or(|only| only == *index))
            .map(|(_, tool)| OaiToolDef {
                tool_type: "function",
                function: OaiToolDefFn {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
            })
            .collect()
    }

    fn build_request<C>(
        &self,
        tools: &[AgentTool<C>],
        visible: Option<usize>,
        messages: Vec<OaiMessage>,
        tool_choice: Option<Value>,
    ) -> OaiChatRequest {
        OaiChatRequest {
            model: self.model.clone(),
            messages,
            tools: Some(Self::tool_defs(tools, visible)),
            tool_choice,
            max_tokens: Some(self.max_tokens),
            parallel_tool_calls: Some(false),
            reasoning_effort: self.reasoning_effort.clone(),
            thinking: if self.thinking_enabled {
                Some(OaiThinking {
                    thinking_type: "enabled",
                })
            } else {
                None
            },
        }
    }

    /// 单 attempt 的 turn 循环。历史由 caller 持有（forced 续跑的续接输入）。
    /// Ok(()) = 终局 accepted；PlainTextEnd = 普通文本提前结束（draft 随 history 一起可用）。
    async fn run_rounds<C: RunCtx>(&self, args: RoundArgs<'_, C>) -> Result<(), RoundEnd> {
        let RoundArgs {
            agent_name,
            tools,
            visible,
            tool_choice,
            history,
            ctx,
            max_turns,
            trace,
        } = args;
        let mut turns = 0usize;
        loop {
            turns += 1;
            if turns > max_turns {
                return Err(RoundEnd::Fatal(AgentRuntimeError::MaxTurns {
                    turns,
                    cap: max_turns,
                }));
            }
            trace.stats.llm_calls += 1;
            trace.write(
                "llm_start",
                json!({"agent": agent_name, "input_item_count": history.len()}),
            );
            let started = Instant::now();
            let request = self.build_request(tools, visible, history.clone(), tool_choice.clone());
            let response = self.chat(&request).await.map_err(RoundEnd::Fatal)?;
            let usage = response.usage.unwrap_or_default();
            trace.stats.input_tokens += usage.prompt_tokens;
            trace.stats.output_tokens += usage.completion_tokens;
            trace.stats.total_tokens += usage.total_tokens;
            trace.write(
                "llm_end",
                json!({
                    "agent": agent_name,
                    "input_tokens": usage.prompt_tokens,
                    "output_tokens": usage.completion_tokens,
                    "total_tokens": usage.total_tokens,
                    "output_item_count": 1,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                }),
            );
            let choice = response.choices.into_iter().next().ok_or(RoundEnd::Fatal(
                AgentRuntimeError::Protocol("response.choices 为空".to_string()),
            ))?;
            let mut message = choice.message;
            if !self.replay_reasoning {
                message.reasoning_content = None;
            }
            let calls = message.tool_calls.clone().unwrap_or_default();
            history.push(message);
            if calls.is_empty() {
                let draft = history
                    .last()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                return Err(RoundEnd::PlainTextEnd(draft));
            }
            for call in &calls {
                trace.stats.tool_calls += 1;
                trace.stats.tool_names.push(call.function.name.clone());
                trace.write(
                    "tool_start",
                    json!({
                        "agent": agent_name,
                        "tool": call.function.name,
                        "tool_call_id": call.id,
                    }),
                );
                let tool_started = Instant::now();
                let result = match tools.iter_mut().find(|t| t.name == call.function.name) {
                    Some(tool) => {
                        let args: Value =
                            serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
                        (tool.handler)(ctx, &args)
                    }
                    None => json!({"error": format!("unknown tool: {}", call.function.name)}),
                };
                trace.write(
                    "tool_end",
                    json!({
                        "agent": agent_name,
                        "tool": call.function.name,
                        "result_chars": result.to_string().len(),
                        "elapsed_ms": tool_started.elapsed().as_millis() as u64,
                    }),
                );
                history.push(OaiMessage::tool_result(call.id.clone(), &result));
                // 终局已接受 → 立即终止（Python terminal_tool_behavior；parallel_tool_calls=false
                // ⇒ 每批实际单调用，提前退出无损）。
                if ctx.slot().value.is_some() {
                    return Ok(());
                }
            }
        }
    }
}

enum RoundEnd {
    PlainTextEnd(String),
    Fatal(AgentRuntimeError),
}

/// [`AgentRuntime::run_rounds`] 参数捆（clippy 七参上限）。
struct RoundArgs<'a, C: RunCtx> {
    agent_name: &'a str,
    tools: &'a mut [AgentTool<C>],
    visible: Option<usize>,
    tool_choice: Option<Value>,
    history: &'a mut Vec<OaiMessage>,
    ctx: &'a mut C,
    max_turns: usize,
    trace: &'a mut Trace,
}

fn is_transient(err: &async_openai::error::OpenAIError) -> bool {
    use async_openai::error::OpenAIError;
    match err {
        OpenAIError::Reqwest(err) => err.is_timeout() || err.is_connect() || err.is_request(),
        _ => false,
    }
}

/// [`run_toolcall_agent`] 的参数包。
pub struct AttemptPlan<'a> {
    pub label: &'a str,
    pub prompt: &'a str,
    pub max_turns: usize,
    pub retries: usize,
    pub backoff_seconds: f64,
}

#[derive(Debug)]
pub struct RunOutcome<S> {
    pub submission: S,
    /// Python parity 断言面：终局接受时恒为 "accepted"。
    pub final_output: String,
}

/// 终局协议主循环（Python `runtime.py:150` `run_toolcall_agent` 的逐行契约）。
pub async fn run_toolcall_agent<C: RunCtx, S: for<'de> Deserialize<'de>>(
    runtime: &AgentRuntime,
    spec: &mut AgentSpec<C>,
    plan: AttemptPlan<'_>,
    ctx: &mut C,
    trace: &mut Trace,
) -> Result<RunOutcome<S>, AgentRuntimeError> {
    let terminal_index = spec
        .terminal_index()
        .ok_or_else(|| AgentRuntimeError::Config(format!("{} 未声明终局工具", spec.name)))?;
    let terminal_name = spec.tools[terminal_index].name.clone();
    let mut last_error = String::new();
    for attempt in 0..=plan.retries {
        if attempt > 0 && plan.backoff_seconds > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(
                plan.backoff_seconds * attempt as f64,
            ))
            .await;
        }
        ctx.slot().value = None;
        ctx.slot().validation_errors.clear();

        let mut history = vec![
            OaiMessage::system(spec.instructions.clone()),
            OaiMessage::user(plan.prompt.to_string()),
        ];
        let main_end = runtime
            .run_rounds(RoundArgs {
                agent_name: &spec.name,
                tools: &mut spec.tools,
                visible: None,
                tool_choice: None,
                history: &mut history,
                ctx,
                max_turns: plan.max_turns,
                trace,
            })
            .await;
        let attempt_error = match main_end {
            Ok(()) => None,
            Err(RoundEnd::Fatal(err)) => Some(err.to_string()),
            Err(RoundEnd::PlainTextEnd(draft)) => {
                // 具名强制重提交（Python 文案逐字）：历史已含草稿，追加 user 追述。
                history.push(OaiMessage::user(format!(
                    "上一轮以普通文本结束，因此不是有效结果。保留前述全部思考、工具调用和工具结果；\
                     现在只能调用 {terminal_name} 提交。上一轮文本草稿：\n{}",
                    truncate_chars(&draft, DRAFT_TRUNCATE_CHARS)
                )));
                let forced_name = format!("{} Forced Terminal Submission", spec.name);
                let forced_turns = plan.max_turns.clamp(FORCED_TURNS_MIN, FORCED_TURNS_CAP);
                let forced_end = runtime
                    .run_rounds(RoundArgs {
                        agent_name: &forced_name,
                        tools: &mut spec.tools,
                        visible: Some(terminal_index),
                        tool_choice: Some(json!({
                            "type": "function",
                            "function": {"name": terminal_name},
                        })),
                        history: &mut history,
                        ctx,
                        max_turns: forced_turns,
                        trace,
                    })
                    .await;
                match forced_end {
                    Ok(()) => None,
                    Err(RoundEnd::Fatal(err)) => Some(err.to_string()),
                    Err(RoundEnd::PlainTextEnd(_)) => {
                        let errors = ctx.slot().validation_errors.clone();
                        Some(
                            AgentRuntimeError::NoTerminal {
                                label: plan.label.to_string(),
                                terminal: terminal_name.clone(),
                                errors,
                            }
                            .to_string(),
                        )
                    }
                }
            }
        };
        if let Some(error) = attempt_error {
            trace.write(
                "run_attempt_failed",
                json!({"label": plan.label, "attempt": attempt + 1, "error": error}),
            );
            last_error = error;
            continue;
        }
        // Ok 路径：终局槽位成立 ⇒ decode 为 S 一并移交。
        let slot_value = ctx.slot().value.clone().ok_or_else(|| {
            AgentRuntimeError::Protocol("accepted 后 submission 槽为空".to_string())
        })?;
        let submission: S = serde_json::from_value(slot_value)
            .map_err(|err| AgentRuntimeError::Protocol(format!("submission decode: {err}")))?;
        return Ok(RunOutcome {
            submission,
            final_output: "accepted".to_string(),
        });
    }
    Err(AgentRuntimeError::Exhausted {
        label: plan.label.to_string(),
        attempts: plan.retries + 1,
        reason: last_error,
    })
}
