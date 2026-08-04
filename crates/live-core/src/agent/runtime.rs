//! Agent 运行时（移植 Python `agent/runtime.py`，design §M3 协议红线）。
//!
//! 体积备书（r8-F2）：808 行贴 800 线——协议合约是一整块概念（终局/重试/回放/trace
//! 四段环环相扣），按子协议拆卷会把一份协议契约撕成四散面；出 850 或协议不变量解体
//! 时再议拆分，自然缝 = 传输层（chat 通道） vs 合约层（终局/重试状态机）。
//!
//! 协议契约（逐条对照 Python `run_toolcall_agent`）：
//! - 唯一终局工具：`ctx.slot().value` 被接受即终止（final_output="accepted"）；
//! - 普通文本提前结束 → 保留全部历史（system/user/assistant 草稿/工具调用与结果），
//!   追述 user 段（草稿截断 [`DRAFT_TRUNCATE_CHARS`]），tools 列表收窄为仅终局 +
//!   `tool_choice` 具名强制重跑，forced max_turns 钳 [`FORCED_TURNS_MIN..=FORCED_TURNS_CAP`]；
//!   两轮仍无 accepted → attempt 失败（`NoTerminal`）计入线性退避重试；
//! - 重试：共 `retries+1` 次 attempt，线性退避 `backoff_seconds * attempt`；HTTP 层瞬时错
//!   （timeout/connect/本轮发送失败/408/409/429/5xx）只在 `chat()` 内层 ≤[`HTTP_EXTRA_ATTEMPTS`]
//!   次压住——429/5xx 形态恢复前提：body 是 OpenAI 错误 JSON；裸文本 body 解析为
//!   JSONDeserialize 形态属非瞬时，单次即败（钉见 agent_runtime.rs）——与 Python
//!   httpx 状态码驱动的差异已入偏差单（r1-M4）；
//! - reasoning_content：BYO 消息结构体全程往返（回放上送）。`replay_reasoning=false` 时
//!   落历史前剥离；**永不写入 trace**；
//! - Trace：JSONL 元数据（time/event/agent/token 三元组/tool 名+tool_call_id/result_chars
//!   /elapsed_ms）——S0 漏埋教训：tool 名与显式耗时是必填字段，不接受 None。
//!
//! 传输 = async-openai `create_byot`：请求与响应均 BYO 类型（ADR 2026-08-04 源码级验证）。
//! 设计与 Python 的已知偏差（书面）：tools 数组在 forced 续跑中收窄为仅终局（Python 同义：
//! forced Agent 只挂终局工具），dispatch 仍按名字查表——比原文献节约一次 handler 迁移问题。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::AiConfig;

use super::throttle::Throttle;

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
    /// Z1/P0-2:DeepSeek prompt-caching 命中/未中计量。缺省=0（非 DeepSeek 或无计数）。
    #[serde(default)]
    pub prompt_cache_hit_tokens: i64,
    #[serde(default)]
    pub prompt_cache_miss_tokens: i64,
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
    /// r1-F1 单 agent token 熔断：累计 total_tokens 超过 budget 后触顶终止。
    /// 前缀 `token_budget` 即失败分类（viewer_failure 缓存 error 面落盘）。
    #[error("token_budget {budget} exceeded: cumulative total_tokens {used}")]
    TokenBudget { budget: u32, used: i64 },
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
    /// Z1/P0-2:prompt-cache 命中/未中累计，cache 观测盒数据源。
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
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

/// Send 界是 M4 pipeline 并发跑多个 viewer agent 的前置（review graph m4）。
/// 闭包捕获均为 String 或无捕获，天然 Send；无此界则 run future 整体为非 Send。
pub type ToolHandler<C> = Box<dyn FnMut(&mut C, &Value) -> Value + Send>;

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

/// 终局校验器三态（R4：基础设施失败与业务拒收分道——Python SDK tool-error 通道镜像）。
/// 此前 infrastructure 失败被白标为「模型可修正的校验拒收」，模型空烧修正轮回。
pub enum TerminalOutcome {
    /// 校验通过 → `accepted:true` + 计数载荷（Python 终局工具返回形状）。
    Accept(Value),
    /// 业务校验拒绝 → Python 三键 `accepted/false+errors+instruction`；submission 槽保持 None。
    Reject(Vec<String>),
    /// 基础设施失败（图 IO/SQLite 等）→ `{"error": ...}`；槽位**不污染**
    /// （Python `validation_errors` 不覆写语义；模型可读错误重试，不被指示"修正"程序故障）。
    Fatal(String),
}

/// 终局工具工厂：serde schema 解析 → 业务校验 → 接受/拒绝/故障。
/// 拒绝形状与 Python tools.py 三终局一致：
/// `{"accepted":false,"errors":[...],"instruction":"修正后重新调用本工具"}`，
/// 且 submission 槽必须保持 None（测试钉）。
pub fn make_terminal_tool<C, S, V>(name: &str, description: &str, validator: V) -> AgentTool<C>
where
    C: RunCtx,
    S: for<'de> Deserialize<'de> + Serialize + schemars::JsonSchema + 'static,
    V: Fn(&mut C, &S) -> TerminalOutcome + Send + 'static,
{
    let schema = schemars::schema_for!(TerminalArgs<S>);
    AgentTool {
        name: name.to_string(),
        description: description.to_string(),
        parameters: serde_json::to_value(schema).expect("schema 构造失败是编译期缺陷"),
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
                    TerminalOutcome::Accept(payload) => {
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
                    TerminalOutcome::Reject(errors) => {
                        let slot = ctx.slot();
                        slot.validation_errors = errors.clone();
                        slot.value = None;
                        json!({
                            "accepted": false,
                            "errors": errors,
                            "instruction": "修正后重新调用本工具",
                        })
                    }
                    TerminalOutcome::Fatal(message) => json!({"error": message}),
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
/// Z3/P0-4：HTTP 内层重试基础退避（秒）。attempt n 的退避为 base * 2^(n-1) + jitter，
/// 封顶 BACKOFF_CAP_SECONDS；Retry-After 头（经 redact 消息信标回传）优先并向下取 max。
const HTTP_BACKOFF_BASE_SECONDS: f64 = 1.0;
const HTTP_BACKOFF_CAP_SECONDS: f64 = 30.0;
/// Retry-After 秒数的可接受上限；超过则放弃重试（避免单 agent 挂死整批）。
const RETRY_AFTER_MAX_SECONDS: f64 = 60.0;

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
    /// Z3/P0-4：全局 LLM 请求漏桶；None = 不限速（config max_llm_rpm=0 默认）。
    throttle: Option<Arc<Throttle>>,
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
        // parity 红线：重试主权唯一属于 chat() 内层（HTTP_EXTRA_ATTEMPTS）。
        // 0.41.3 默认 ReqwestExecutor 内嵌 OpenAIRetryLayer(max_retries=3)——
        // 必须显式换成纯传输 ReqwestService，否则瞬时错请求数 ×4（M4-C 实测 12/agent）。
        // r1-N1：with_http_service 只换 executor、不换 request_client——请求仍由内部
        // 默认 client 构造；超时等行为唯一由本 service client 在 execute 阶段决定。
        Ok(Self::build(
            async_openai::Client::with_config(config)
                .with_http_service(async_openai::middleware::ReqwestService::new(http)),
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
        // 同 from_ai_config：禁默认 executor 隐藏重试层（见该处注释）。
        // r1-M5：裸 client 必须有超时护栏——否则「挂起 server」剧本永不返回，
        // 失真地挂死测试本体；30s 与现存 wiremock delay 剧本（毫秒级）相容。
        let http = reqwest13::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("for_test client build");
        Self::build(
            async_openai::Client::with_config(config)
                .with_http_service(async_openai::middleware::ReqwestService::new(http)),
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
            throttle: None,
        }
    }

    /// Z3/P0-4：挂载 run 级共享漏桶。同一 Arc 在 viewer 任务间克隆共享，
    /// 故全 run 的出队 LLM 请求（全部 agent 的每一轮）合并限速。
    pub fn with_throttle(mut self, throttle: Arc<Throttle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// 单次 chat + 瞬时重试。非流式 + 网关超时：以 retry 兜住（S0 实测 0 失败）。
    /// Z3/P0-4：过闸放行后才出网（许可即请求，1:1）；退避升级为指数 + 全区间抖动，
    /// 429/503 携带 Retry-After 时取其秒数（封顶 RETRY_AFTER_MAX_SECONDS）与指数退避取大者。
    pub async fn chat(
        &self,
        request: &OaiChatRequest,
    ) -> Result<OaiChatResponse, AgentRuntimeError> {
        if let Some(throttle) = &self.throttle {
            throttle.acquire().await;
        }
        let mut last_error: Option<async_openai::error::OpenAIError> = None;
        let mut retry_after: Option<f64> = None;
        for attempt in 0..=HTTP_EXTRA_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(http_backoff(attempt, retry_after)).await;
            }
            match self.client.chat().create_byot(request).await {
                Ok(response) => return Ok(response),
                Err(err) => {
                    if !is_transient(&err) || attempt == HTTP_EXTRA_ATTEMPTS {
                        // 安全 M1：原文可能携带响应体/key 片段 → 脱敏后进错误面（红线 §11）。
                        return Err(AgentRuntimeError::Transport(
                            super::redact::redact_openai_error(&err),
                        ));
                    }
                    retry_after = retry_after_seconds(&err);
                    last_error = Some(err);
                }
            }
        }
        Err(AgentRuntimeError::Transport(
            super::redact::redact_openai_error(&last_error.expect("loop 至少一次")),
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
    // C: Send 的前置要求升格自此：工具 handler 卸到 scoped std::thread 上执行
    // （&mut C 跨线程借用），viewer agent ctx 本就满足于 JoinSet::spawn 的 Send 界。
    async fn run_rounds<C: RunCtx + Send>(&self, args: RoundArgs<'_, C>) -> Result<(), RoundEnd> {
        let RoundArgs {
            agent_name,
            tools,
            visible,
            tool_choice,
            history,
            ctx,
            max_turns,
            token_budget,
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
            // Z1/P0-2:prompt-cache 命中/未中累计
            trace.stats.cache_hit_tokens += usage.prompt_cache_hit_tokens;
            trace.stats.cache_miss_tokens += usage.prompt_cache_miss_tokens;
            // r1-F1 熔断点：每轮 LLM 请求后核对累计 total_tokens，超预算即触顶终止。
            // 顺既有 Fatal 通道（不发新错误类）；run_toolcall_agent 对 TokenBudget 特判不重试。
            if let Some(budget) = token_budget
                && trace.stats.total_tokens > budget as i64
            {
                return Err(RoundEnd::Fatal(AgentRuntimeError::TokenBudget {
                    budget,
                    used: trace.stats.total_tokens,
                }));
            }
            trace.write(
                "llm_end",
                json!({
                    "agent": agent_name,
                    "input_tokens": usage.prompt_tokens,
                    "output_tokens": usage.completion_tokens,
                    "total_tokens": usage.total_tokens,
                    "cache_hit_tokens": usage.prompt_cache_hit_tokens,
                    "cache_miss_tokens": usage.prompt_cache_miss_tokens,
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
                // forced(visible=Some) 期间 dispatch 面同步收窄（Python forced agent
                // tools=[terminal_tool]，非终局名落 SDK "not found" 文本，逐字对齐）。
                let routed = match visible {
                    Some(only) => tools
                        .get_mut(only)
                        .filter(|tool| tool.name == call.function.name),
                    None => tools.iter_mut().find(|t| t.name == call.function.name),
                };
                let result = match routed {
                    Some(tool) => match serde_json::from_str(&call.function.arguments) {
                        // W2-C1（M6-C② 实战炸出）：工具 handler 必须在「无 tokio 上下文」的
                        // 线程上执行。reqwest::blocking 的 wait.rs enter() 在 debug 构建下
                        // 会为每次阻塞调用临时建/丢一个壳 runtime 做误脚枪检查——在 async
                        // ctx 内执行即 panic「Cannot drop a runtime…」。scoped std::thread：
                        // 干净上下文 + 借用安全（C: Send）+ 与 inline 同序的执行语义。
                        Ok(args) => std::thread::scope(|scope| {
                            scope
                                .spawn(|| (tool.handler)(ctx, &args))
                                .join()
                                .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
                        }),
                        // 非法 arguments：不执行 handler，回喂可读错误让模型自愈（工程 m2）。
                        Err(err) => json!({"error": format!("invalid tool arguments: {err}")}),
                    },
                    None => json!({"error": format!("Tool '{}' not found.", call.function.name)}),
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
    /// r1-F1：None=不设预算；Some(budget)=累计 total_tokens 超限即熔断。
    token_budget: Option<u32>,
    trace: &'a mut Trace,
}

fn is_transient(err: &async_openai::error::OpenAIError) -> bool {
    use async_openai::error::OpenAIError;
    match err {
        OpenAIError::Reqwest(err) => err.is_timeout() || err.is_connect() || err.is_request(),
        // Python openai SDK `_should_retry` 同型：408/409/429/5xx 内层重试（≤ HTTP_EXTRA_ATTEMPTS）。
        OpenAIError::ApiError(resp) => {
            matches!(resp.status_code.as_u16(), 408 | 409 | 429)
                || resp.status_code.is_server_error()
        }
        _ => false,
    }
}

/// Z3/P0-4：内层重试退避 = 指数（base·2^(attempt-1)，封顶 30s）× 全区间抖动。
/// Retry-After 提供时与指数退避取大者。抖动由 attempt 索引确定的轻量哈希产生——
/// 无 RNG 依赖、单进程内可复现；测试可用 VTD_LIVE_CORE_TEST_JITTER_SEED 固定为 0 抖动。
fn http_backoff(attempt: usize, retry_after: Option<f64>) -> Duration {
    let exp = HTTP_BACKOFF_BASE_SECONDS
        * 2f64
            .powi(attempt.saturating_sub(1) as i32)
            .min(HTTP_BACKOFF_CAP_SECONDS);
    let jitter = jitter_fraction(attempt);
    let backoff = exp * jitter;
    let seconds = match retry_after {
        Some(hint) => backoff.max(hint),
        None => backoff,
    };
    Duration::from_secs_f64(seconds)
}

/// 全区间抖动系数 ∈ [0, 1]；环境变量 VTD_LIVE_CORE_TEST_JITTER_SEED 存在时恒 0（测试确定性）。
fn jitter_fraction(attempt: usize) -> f64 {
    if std::env::var_os("VTD_LIVE_CORE_TEST_JITTER_SEED").is_some() {
        return 0.0;
    }
    // SplitMix64 一步：以 attempt 与进程 id 混合，低成本去同步化，避免惊群同步重试。
    let mut state = (attempt as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(std::process::id() as u64);
    state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let mixed = state ^ (state >> 31);
    (mixed >> 11) as f64 / (1u64 << 53) as f64
}

/// Z3/P0-4：从 429/503 错误提取 Retry-After 秒数。
/// async-openai 的 ApiError 不暴露响应头，退而求其次：redact 已在消息中保留
/// "Retry-After: <秒>" 片段时解析之；仅 429/503 状态尝试（其他状态无该语义）。
fn retry_after_seconds(err: &async_openai::error::OpenAIError) -> Option<f64> {
    use async_openai::error::OpenAIError;
    let OpenAIError::ApiError(resp) = err else {
        return None;
    };
    if !matches!(resp.status_code.as_u16(), 429 | 503) {
        return None;
    }
    let message = super::redact::redact_openai_error(err);
    let marker = "Retry-After:";
    let start = message.find(marker)? + marker.len();
    let token: String = message[start..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let seconds: f64 = token.parse().ok()?;
    (seconds <= RETRY_AFTER_MAX_SECONDS).then_some(seconds)
}

/// [`run_toolcall_agent`] 的参数包。
pub struct AttemptPlan<'a> {
    pub label: &'a str,
    pub prompt: &'a str,
    pub max_turns: usize,
    pub retries: usize,
    pub backoff_seconds: f64,
    /// r1-F1：None=不设预算；Some(budget)=该 agent 累计 total_tokens 超限即熔断终止。
    pub token_budget: Option<u32>,
}

#[derive(Debug)]
pub struct RunOutcome<S> {
    pub submission: S,
    /// Python parity 断言面：终局接受时恒为 "accepted"。
    pub final_output: String,
}

/// 终局协议主循环（Python `runtime.py:150` `run_toolcall_agent` 的逐行契约）。
pub async fn run_toolcall_agent<C: RunCtx + Send, S: for<'de> Deserialize<'de>>(
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
        let main = runtime
            .run_rounds(RoundArgs {
                agent_name: &spec.name,
                tools: &mut spec.tools,
                visible: None,
                tool_choice: None,
                history: &mut history,
                ctx,
                max_turns: plan.max_turns,
                token_budget: plan.token_budget,
                trace,
            })
            .await;
        let attempt_error = match main {
            Ok(()) => None,
            // r1-F1：熔断是终局地——不是可重试的瞬时/协议失败；立即原样抛出。
            Err(RoundEnd::Fatal(err)) => match err {
                AgentRuntimeError::TokenBudget { .. } => return Err(err),
                other => Some(other.to_string()),
            },
            Err(RoundEnd::PlainTextEnd(draft)) => {
                // 具名强制重提交（Python runtime.py:191-224 逐字）：
                // ① forced agent 的 instructions **替换** system 首条（主 instructions 丢弃）；
                // ② 历史已含草稿，追加 user 追述。两点都进 wire，model 才看得到强制语。
                history[0] = OaiMessage::system(format!(
                    "上一轮没有通过终局工具提交，因此不是有效结果。重新阅读完整输入和上一轮普通文本，\
                     现在只能调用 {terminal_name}。必须修正所有校验问题并通过该工具提交。"
                ));
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
                        token_budget: plan.token_budget,
                        trace,
                    })
                    .await;
                match forced_end {
                    Ok(()) => None,
                    // r1-F1：forced 续跑同样受预算约束，触顶即抛（不重试）。
                    Err(RoundEnd::Fatal(err)) => match err {
                        AgentRuntimeError::TokenBudget { .. } => return Err(err),
                        other => Some(other.to_string()),
                    },
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
