//! 安全批：LLM 传输错误的脱敏。
//!
//! 红线（AGENTS.md 明确禁止）：reasoning_content 与 api_key 片段永不进 trace/日志/终态。
//! 事故路径：async-openai 的 `OpenAIError::JSONDeserialize` Display 全量携带响应体
//! （网关把 200 改成坏形状时，body 中已生成的 assistant/reasoning 文本随错误原文
//! 进 trace.jsonl 与 stdout）；`ApiErrorResponse` 透传服务端 message
//! （401 惯常回吐 key 片段）；`StreamError::UnknownEvent` Debug 回显 SSE payload。
//! 本模块把 OpenAIError 压成 "kind + 状态码 + 截断脱敏 message" 三件套，
//! 与原 `BilibiliError::NotJson{endpoint}` 刻意丢 body 的设计同型。

use async_openai::error::OpenAIError;

/// 截断上限（message 只需足以辨认错误类别；body/payload 永远不抄）。
pub const REDACT_MESSAGE_MAX_CHARS: usize = 120;

/// OpenAIError → 脱敏描述。只保留：类别名、HTTP 状态码、`sk-` 打头片段打码后的短 message。
/// 凡 Display 可能携带响应体/SSE payload 的变体，只留类别词，不抄原文。
pub fn redact_openai_error(err: &OpenAIError) -> String {
    match err {
        // body 全文可能被响应体污染 → 完全丢弃，只留类别。
        OpenAIError::JSONDeserialize(..) => {
            "failed to deserialize api response (body redacted)".to_string()
        }
        OpenAIError::ApiError(resp) => format!(
            "api error {}: {} (code: {})",
            resp.status_code,
            mask_secrets(&resp.api_error.message),
            resp.api_error.code.as_deref().unwrap_or("")
        ),
        OpenAIError::Reqwest(req) => format!(
            "http transport: {}",
            if req.is_timeout() {
                "timeout"
            } else if req.is_connect() {
                "connect"
            } else if req.is_request() {
                "request build"
            } else {
                "other"
            }
        ),
        // UnknownEvent 的 Display Debug 回显 SSE 事件 payload → 类别化，不抄。
        OpenAIError::StreamError(_) => "stream error (payload redacted)".to_string(),
        other => format!("openai error: {}", mask_secrets(&other.to_string())),
    }
}

/// `sk-` 打头的 key 片段打码；截断到可诊断长度。
fn mask_secrets(message: &str) -> String {
    let mut masked = String::new();
    let mut rest = message;
    while let Some(index) = rest.find("sk-") {
        masked.push_str(&rest[..index]);
        masked.push_str("sk-***");
        rest = &rest[index + 3..];
        // 跳过 key 本体（字母数字与连字符），保留其后的正常文本。
        rest = rest.trim_start_matches(|ch: char| ch.is_ascii_alphanumeric() || "-_".contains(ch));
    }
    masked.push_str(rest);
    crate::episodes::char_prefix(&masked, REDACT_MESSAGE_MAX_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::error::ApiErrorResponse;

    fn api_err(message: &str) -> OpenAIError {
        OpenAIError::ApiError(ApiErrorResponse {
            status_code: reqwest13::StatusCode::UNAUTHORIZED,
            api_error: async_openai::error::ApiError {
                message: message.to_string(),
                r#type: Some("invalid_request_error".to_string()),
                param: None,
                code: Some("invalid_api_key".to_string()),
            },
        })
    }

    #[test]
    fn api_error_message_masked_and_truncated() {
        let out = redact_openai_error(&api_err("Incorrect API key provided: sk-abcd1234efgh5678"));
        assert!(out.contains("sk-***"), "{out}");
        assert!(!out.contains("abcd1234"), "{out}");
        assert!(out.contains("401"), "{out}");
        assert!(out.contains("invalid_api_key"), "{out}");
    }

    #[test]
    fn json_deserialize_drops_body() {
        let err = OpenAIError::JSONDeserialize(
            serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
            "raw body with reasoning_content leaked".to_string(),
        );
        let out = redact_openai_error(&err);
        assert!(!out.contains("reasoning_content"), "{out}");
        assert!(out.contains("redacted"), "{out}");
    }
}
