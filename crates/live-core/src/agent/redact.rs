//! 安全批：LLM 传输错误的脱敏唯一件。
//!
//! 红线（AGENTS.md 明确禁止）：reasoning_content 与 api_key 片段永不进 trace/日志/终态。
//! 2026-08-13 删码刀11 前本卷主件是 OpenAIError 分类压制——手驾流式后错误面收成
//! ChatWireError 三分（transport.rs），本卷只留文本级出口：sk- 打码 + 截断。
//! 2xx 坏形状照旧固定文案（body 一个字都不抄），与非流式时代 JSONDeserialize 臂同型。

/// 截断上限（message 只需足以辨认错误类别；body/payload 永远不抄）。
pub const REDACT_MESSAGE_MAX_CHARS: usize = 120;

/// 文本级脱敏：`sk-` 打头的 key 片段打码；截断到可诊断长度。
/// 唯一出口——任何要进错误面/trace 的服务端原文都必须过本函数。
pub fn scrub_text(message: &str) -> String {
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

    #[test]
    fn key_fragment_masked_and_truncated() {
        let out = scrub_text("Incorrect API key provided: sk-abcd1234efgh5678");
        assert!(out.contains("sk-***"), "{out}");
        assert!(!out.contains("abcd1234"), "{out}");
    }

    #[test]
    fn truncation_caps_message_length() {
        let out = scrub_text(&"x".repeat(500));
        assert_eq!(out.chars().count(), REDACT_MESSAGE_MAX_CHARS, "{out}");
    }
}
