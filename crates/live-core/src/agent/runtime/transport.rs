//! 传输层（chat 通道）：BYO wire 类型 + HTTP 瞬时错回环。
//!
//! 自 `runtime.rs` 按头注自书缝拆出（「出 850 再议」已破：996 行）——
//! wire 形状对齐 Python agents SDK 实际出网 JSON；瞬时错判定/退避/抖动脉络
//! 与 chat() 的调用语义见 runtime.rs 头注，本卷只承载通道件，不承合约语义。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
pub(super) struct OaiToolDef {
    #[serde(rename = "type")]
    pub(super) tool_type: &'static str, // "function"
    pub(super) function: OaiToolDefFn,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OaiToolDefFn {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: Value,
}

#[derive(Debug, Serialize)]
pub struct OaiChatRequest {
    pub(super) model: String,
    pub(super) messages: Vec<OaiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<OaiToolDef>>,
    /// P2-δ：已删除 tool_choice。DeepSeek 全部模型（v4 线 + reasoner 旧别名）
    /// 均 400 拒 tool_choice:auto 以外的取值——具名/required 是物理不可用管道；
    /// opencode 等成熟 agent 显式只用 auto。
    /// 钳制改由「prompt 收窄 + 重打循环」承担。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parallel_tool_calls: Option<bool>,
    /// Python ModelSettings.reasoning → chat completions 的 reasoning_effort 槽位。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_effort: Option<String>,
    /// Python `extra_body={"thinking":{"type":"enabled"}}` 顶层展平位。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) thinking: Option<OaiThinking>,
    /// 2026-08-13 删码刀11：恒 true——流式是 900s 空闲墙的唯一解法（非流式
    /// 响应体生成期零字节，链路空闲超时准点斩连；SSE 逐 token 带字节流动）。
    pub(super) stream: bool,
    /// 末块携带 usage（支出账实耗真相源）—— relay 不透传则 usage=None 诚实缺，
    /// 绝不本地臆造 token 数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stream_options: Option<OaiStreamOptions>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OaiThinking {
    #[serde(rename = "type")]
    pub(super) thinking_type: &'static str, // "enabled"
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OaiStreamOptions {
    pub(super) include_usage: bool,
}

// ---------------------------------------------------------------------------
// 流式分块重组装入面（chat.completion.chunk → 非流式等价物；只取重组装所需键）
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub(super) struct OaiChunkDeltaToolFn {
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) arguments: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct OaiChunkDeltaToolCall {
    #[serde(default)]
    pub(super) index: usize,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) function: Option<OaiChunkDeltaToolFn>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct OaiChunkDelta {
    #[serde(default)]
    pub(super) content: Option<String>,
    #[serde(default)]
    pub(super) reasoning_content: Option<String>,
    #[serde(default)]
    pub(super) tool_calls: Option<Vec<OaiChunkDeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OaiChunkChoice {
    #[serde(default)]
    pub(super) delta: OaiChunkDelta,
    #[serde(default)]
    pub(super) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OaiChunk {
    #[serde(default)]
    pub(super) choices: Vec<OaiChunkChoice>,
    /// stream_options.include_usage=true 时的末块 usage；relay 不透传则 None。
    #[serde(default)]
    pub(super) usage: Option<OaiUsage>,
}

// ---------------------------------------------------------------------------
// chat_once 线级错误（手驾流式后的瞬时判定唯一源；2026-08-13 前 = OpenAIError 同职责）
// ---------------------------------------------------------------------------

/// 失败三分：HTTP 非 2xx（状态码驱动内层重试——与 Python httpx 同口径；本刀从
/// async-openai「body 能解出 OpenAI 错误 JSON 才重试」的怪异口径摆回）；传输层
/// （timeout/connect/流中断）恒瞬时；2xx 形状坏单次即败（同旧 JSONDeserialize 判）。
/// 文本出口一律已过 redact::scrub_text（key 片段打码 + 120 字截断）。
pub(super) enum ChatWireError {
    Api {
        status: reqwest13::StatusCode,
        message: String,
        code: String,
        retry_after: Option<f64>,
    },
    Transport(&'static str),
    Shape(&'static str),
}

impl ChatWireError {
    pub(super) fn is_transient(&self) -> bool {
        match self {
            // Python openai SDK `_should_retry` 同型：408/409/429/5xx 内层重试。
            Self::Api { status, .. } => {
                matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error()
            }
            Self::Transport(_) => true,
            Self::Shape(_) => false,
        }
    }

    pub(super) fn retry_after(&self) -> Option<f64> {
        match self {
            Self::Api { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    pub(super) fn redact(&self) -> String {
        match self {
            // 与 async-openai 旧字面同款（「api error 502 Bad Gateway:  (code: )」）——
            // 生产判读肌肉记忆与既有钉件双留，勿动形。
            Self::Api {
                status,
                message,
                code,
                ..
            } => format!("api error {status}: {message} (code: {code})"),
            Self::Transport(kind) => format!("http transport: {kind}"),
            Self::Shape(kind) => format!("failed to deserialize api response ({kind} redacted)"),
        }
    }
}

impl ChatWireError {
    /// 非 2xx body → Api：error.message/code 拾取（缺 = 空串，与生产实录
    /// 「api error 502 Bad Gateway:  (code: )」同形）；message 当即脱敏——
    /// 401 惯常回吐 key 片段（红线 §11）。
    pub(super) fn api(status: reqwest13::StatusCode, retry_after: Option<f64>, body: &str) -> Self {
        let parsed = serde_json::from_str::<Value>(body).ok();
        let pick = |path: &str| {
            parsed
                .as_ref()
                .and_then(|value| value.pointer(path))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        Self::Api {
            status,
            message: crate::agent::redact::scrub_text(&pick("/error/message")),
            code: pick("/error/code"),
            retry_after,
        }
    }
}

/// reqwest 错误 → 四分类（与 async-openai 旧字面同款——错误面钉件/判读记忆双留）。
pub(super) fn transport_kind(err: &reqwest13::Error) -> &'static str {
    if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else if err.is_request() {
        "request build"
    } else {
        "other"
    }
}

/// SSE 重组装器：content/reasoning 逐块拼、tool_calls 按 index 跨块合流、
/// usage 取末块、[DONE]/finish_reason 收旗——产物与非流式等键等价。
#[derive(Default)]
pub(super) struct StreamAssembly {
    content: String,
    reasoning: String,
    calls: Vec<OaiToolCall>,
    pub(super) finish_reason: Option<String>,
    usage: Option<OaiUsage>,
    pub(super) done: bool,
}

impl StreamAssembly {
    fn absorb(&mut self, chunk: OaiChunk) {
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }
        for choice in chunk.choices {
            if choice.finish_reason.is_some() {
                self.finish_reason = choice.finish_reason;
            }
            let delta = choice.delta;
            if let Some(text) = delta.content {
                self.content.push_str(&text);
            }
            if let Some(text) = delta.reasoning_content {
                self.reasoning.push_str(&text);
            }
            for call in delta.tool_calls.unwrap_or_default() {
                if self.calls.len() <= call.index {
                    self.calls.resize_with(call.index + 1, || OaiToolCall {
                        id: String::new(),
                        tool_type: "function".to_string(),
                        function: OaiToolFn {
                            name: String::new(),
                            arguments: String::new(),
                        },
                    });
                }
                let slot = &mut self.calls[call.index];
                if let Some(id) = call.id {
                    slot.id = id;
                }
                if let Some(function) = call.function {
                    if let Some(name) = function.name {
                        slot.function.name = name;
                    }
                    if let Some(arguments) = function.arguments {
                        slot.function.arguments.push_str(&arguments);
                    }
                }
            }
        }
    }

    pub(super) fn finish(self) -> OaiChatResponse {
        let calls: Vec<OaiToolCall> = self
            .calls
            .into_iter()
            .filter(|call| !call.id.is_empty() || !call.function.name.is_empty())
            .collect();
        OaiChatResponse {
            choices: vec![OaiChoice {
                message: OaiMessage {
                    role: "assistant".to_string(),
                    // 流式分块不存在即缺席=空——与非流式 null/缺席同义，下游
                    // 「content: Option」消费面零分叉。
                    content: (!self.content.is_empty()).then_some(self.content),
                    reasoning_content: (!self.reasoning.is_empty()).then_some(self.reasoning),
                    tool_calls: (!calls.is_empty()).then_some(calls),
                    tool_call_id: None,
                },
                finish_reason: self.finish_reason,
            }],
            usage: self.usage,
        }
    }
}

/// 单 SSE 事件块（已按 \n\n 定界的完整事件）→ 逐行 data 吸收。
/// [DONE] 置终旗；畸形 data json 判 Shape（非瞬时——垃圾该快败，同旧
/// StreamError 非瞬时判）。
pub(super) fn absorb_event(
    assembly: &mut StreamAssembly,
    event: &[u8],
) -> Result<(), ChatWireError> {
    let text = String::from_utf8_lossy(event);
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            assembly.done = true;
            continue;
        }
        let chunk: OaiChunk =
            serde_json::from_str(data).map_err(|_| ChatWireError::Shape("sse event"))?;
        assembly.absorb(chunk);
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
pub struct OaiUsage {
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
    /// DeepSeek prompt-caching 命中/未中计量。缺省=0（非 DeepSeek 或无计数）。
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
// HTTP 瞬时错回环（chat() 内层重试的全部判定件）
// ---------------------------------------------------------------------------

/// HTTP 层瞬时错误附加尝试。
/// 2026-08-05 生产事故定标：自建 dsv4 集群冷启动期的 502 Bad Gateway 风暴
/// （网关 ~16s 超时一拍，22 viewer 死 16）仅靠 Python 等价的 2 次附加尝试
/// 活不过去——加宽到 4，最坏叠加退避 ≈1+2+4+8s（封顶 30s）×抖动，
/// 单次 chat 的最差吸收窗 ≈15s+请求往返×5，足以活过冷加载分钟级窗口。
pub(super) const HTTP_EXTRA_ATTEMPTS: usize = 4;
/// HTTP 内层重试基础退避（秒）。attempt n 的退避为 base * 2^(n-1) + jitter，
/// 封顶 BACKOFF_CAP_SECONDS；Retry-After 头（手驾后直读，无需消息信标）向下取 max。
const HTTP_BACKOFF_BASE_SECONDS: f64 = 1.0;
const HTTP_BACKOFF_CAP_SECONDS: f64 = 30.0;
/// Retry-After 秒数的可接受上限；超过则放弃重试（避免单 agent 挂死整批）。
pub(super) const RETRY_AFTER_MAX_SECONDS: f64 = 60.0;

/// 内层重试退避 = 指数（base·2^(attempt-1)，封顶 30s）× 全区间抖动。
/// Retry-After 提供时与指数退避取大者。抖动由 attempt 索引确定的轻量哈希产生——
/// 无 RNG 依赖、单进程内可复现；测试可用 VTD_LIVE_CORE_TEST_JITTER_SEED 固定为 0 抖动。
pub(super) fn http_backoff(attempt: usize, retry_after: Option<f64>) -> Duration {
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
