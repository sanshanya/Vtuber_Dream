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
}

#[derive(Debug, Clone, Serialize)]
pub struct OaiThinking {
    #[serde(rename = "type")]
    pub(super) thinking_type: &'static str, // "enabled"
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
/// 封顶 BACKOFF_CAP_SECONDS；Retry-After 头（经 redact 消息信标回传）优先并向下取 max。
const HTTP_BACKOFF_BASE_SECONDS: f64 = 1.0;
const HTTP_BACKOFF_CAP_SECONDS: f64 = 30.0;
/// Retry-After 秒数的可接受上限；超过则放弃重试（避免单 agent 挂死整批）。
const RETRY_AFTER_MAX_SECONDS: f64 = 60.0;

pub(super) fn is_transient(err: &async_openai::error::OpenAIError) -> bool {
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

/// 从 429/503 错误提取 Retry-After 秒数。
/// async-openai 的 ApiError 不暴露响应头，退而求其次：redact 已在消息中保留
/// "Retry-After: <秒>" 片段时解析之；仅 429/503 状态尝试（其他状态无该语义）。
pub(super) fn retry_after_seconds(err: &async_openai::error::OpenAIError) -> Option<f64> {
    use async_openai::error::OpenAIError;
    let OpenAIError::ApiError(resp) = err else {
        return None;
    };
    if !matches!(resp.status_code.as_u16(), 429 | 503) {
        return None;
    }
    let message = crate::agent::redact::redact_openai_error(err);
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
