//! integration-test 共享剧本基建（旧两份复制的漂移源收口）。
//! 各集成测试文件经 `mod common;` 链入——head 注不列消费名单（列了就漂）。

#![allow(dead_code)]

use serde_json::Value;
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

/// 请求体谓词匹配器（wiremock 同 path 叠 mock = 最早挂载优先，靠本谓词按回合 gate）。
pub struct BodyPred<F>(pub F)
where
    F: Fn(&Value) -> bool + Send + Sync;

impl<F> Match for BodyPred<F>
where
    F: Fn(&Value) -> bool + Send + Sync,
{
    fn matches(&self, request: &Request) -> bool {
        match serde_json::from_slice::<Value>(&request.body) {
            Ok(body) => (self.0)(&body),
            Err(_) => false,
        }
    }
}

pub fn messages_len(n: usize) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    move |body: &Value| body["messages"].as_array().is_some_and(|m| m.len() == n)
}

pub fn replayed_reasoning(expect: &str) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    let expect = expect.to_string();
    move |body: &Value| {
        body["messages"].as_array().is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m["role"].as_str() == Some("assistant")
                    && m["reasoning_content"].as_str() == Some(expect.as_str())
            })
        })
    }
}

/// 断言 assistant 历史消息中**全都不含** reasoning_content 键（剥离开关验证）。
pub fn no_reasoning_replayed() -> impl Fn(&Value) -> bool + Send + Sync {
    |body: &Value| {
        body["messages"].as_array().is_some_and(|msgs| {
            let assistants: Vec<&Value> = msgs
                .iter()
                .filter(|m| m["role"].as_str() == Some("assistant"))
                .collect();
            !assistants.is_empty()
                && assistants
                    .iter()
                    .all(|m| m.get("reasoning_content").is_none())
        })
    }
}

/// reasoning 归属钉：有 reasoning 的 assistant 消息必须紧跟自己 tool_calls 的结果。
pub fn reasoning_attribution() -> impl Fn(&Value) -> bool + Send + Sync {
    |body: &Value| {
        body["messages"].as_array().is_some_and(|msgs| {
            msgs.iter().enumerate().all(|(index, message)| {
                if message["role"].as_str() != Some("assistant")
                    || message["reasoning_content"].is_null()
                {
                    return true;
                }
                let id = message["tool_calls"][0]["id"].as_str();
                id.is_some()
                    && msgs.get(index + 1).is_some_and(|tool| {
                        tool["role"].as_str() == Some("tool") && tool["tool_call_id"].as_str() == id
                    })
            })
        })
    }
}

/// parallel_tool_calls=false 的 wire 钉（剧本计数 messages_len(2k) 的隐性前提）。
pub fn parallel_calls_disabled() -> impl Fn(&Value) -> bool + Send + Sync {
    |body: &Value| body["parallel_tool_calls"] == Value::Bool(false)
}

pub fn assistant_tool_call(id: &str, name: &str, args: Value, reasoning: Option<&str>) -> Value {
    serde_json::json!({
        "id": format!("chatcmpl-{id}"),
        "object": "chat.completion",
        "created": 1_700_000_000,
        "model": "custom-reasoning-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning_content": reasoning,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": args.to_string()},
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
}

pub fn assistant_text(text: &str) -> Value {
    serde_json::json!({
        "id": "chatcmpl-text",
        "object": "chat.completion",
        "created": 1_700_000_000,
        "model": "custom-reasoning-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
}

/// 自定义计费响应（token 熔断剧本：单回合 burn 穿预算的 usage）。
pub fn assistant_bill(text: &str, total_tokens: i64) -> Value {
    let mut value = assistant_text(text);
    value["usage"] = serde_json::json!({
        "prompt_tokens": total_tokens / 2,
        "completion_tokens": total_tokens - total_tokens / 2,
        "total_tokens": total_tokens,
    });
    value
}

/// 挂一个恰好被请求 1 次的回合 mock。
pub async fn mount_turn(
    server: &MockServer,
    predicate: impl Fn(&Value) -> bool + Send + Sync + 'static,
    response: Value,
) {
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .and(BodyPred(predicate))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

// P2-δ：mount_tool_choice_rejected_400 已删——DeepSeek 拒 tool_choice 行为是
// 「只放行 auto」的永远形态，多做一次 P2-β 误报也无意义；等相关观测改签
// 由「超时/瞬时」挂模直接接走，不依赖具名工具路由剧本。

/// completion JSON → 拟真 SSE 文本（message → 单 delta + finish_reason 空块
/// ＋usage 末块＋[DONE]）。钉件手搓分块序列之外的兜底转换器——生产语义：
/// 流式重组装面与非流式响应面等键等价（删码刀11）。
pub fn sse_of(completion: Value) -> String {
    let choice0 = &completion["choices"][0];
    let delta = serde_json::json!({
        "content": choice0["message"]["content"],
        "reasoning_content": choice0["message"]["reasoning_content"],
        "tool_calls": choice0["message"]["tool_calls"],
    });
    let mut out = String::new();
    out.push_str("data: ");
    out.push_str(
        &serde_json::json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]})
            .to_string(),
    );
    out.push_str("\n\n");
    out.push_str("data: ");
    out.push_str(
        &serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":choice0["finish_reason"]}]})
            .to_string(),
    );
    out.push_str("\n\n");
    if let Some(usage) = completion.get("usage").filter(|usage| !usage.is_null()) {
        out.push_str("data: ");
        out.push_str(&serde_json::json!({"choices":[],"usage":usage}).to_string());
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    out
}
