//! M3-A 协议 fixture 测试：终局 Tool Call 协议 + reasoning_content 往返回放。
//!
//! 剧本对齐 Python `tests/test_ai_agent.py` 的 httpx.MockTransport 计数剧本：
//! - turn 1/2/3 的 assistant 消息逐轮携带不同 reasoning_content；
//! - matcher 只做路由（messages 长度 + 关键字段锚定），断言统一在
//!   `server.received_requests()` 捕获底账上完成（wiremock 拒绝即 404，
//!   会把协议偏差变成 Transport 错——真正的协议断言必须落在捕获请求上）。

use live_core::agent::probe::{ProbeContext, probe_spec};
use live_core::agent::runtime::{
    AgentRuntime, AttemptPlan, DRAFT_TRUNCATE_CHARS, Trace, run_toolcall_agent, truncate_chars,
};
use live_core::models::ProbeResult;
use serde_json::{Value, json};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

/// wiremock 自定义 matcher：按请求体 JSON 谓词路由（剧本分流器）。
struct BodyPred<F>(F)
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

fn assistant_tool_call(id: &str, name: &str, args: Value, reasoning: Option<&str>) -> Value {
    json!({
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

fn assistant_text(text: &str) -> Value {
    json!({
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

async fn mount_turn(
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

/// 断言 assistant 历史消息里存在一条 reasoning_content 等于期望值（逐字节）。
fn replayed_reasoning(expect: &str) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
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

fn messages_len(n: usize) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    move |body: &Value| body["messages"].as_array().is_some_and(|m| m.len() == n)
}

/// 断言 assistant 历史消息中不含 reasoning_content 键（剥离开关验证）。
fn no_reasoning_replayed() -> impl Fn(&Value) -> bool + Send + Sync {
    |body: &Value| {
        body["messages"].as_array().is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m["role"].as_str() == Some("assistant") && m.get("reasoning_content").is_none()
            })
        })
    }
}

fn test_runtime(server: &MockServer) -> AgentRuntime {
    AgentRuntime::for_test(&server.uri(), "custom-reasoning-model", 131_072, true, true)
}

fn plan<'a>(prompt: &'a str, max_turns: usize) -> AttemptPlan<'a> {
    AttemptPlan {
        label: "agent-check",
        prompt,
        max_turns,
        retries: 0,
        backoff_seconds: 0.0,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reasoning_replay_and_terminal_tool_call() {
    let server = MockServer::start().await;
    // Python test_ai_agent.py:62 剧本：取种子 → 乘法 → 终局提交，三轮。
    mount_turn(
        &server,
        messages_len(2),
        assistant_tool_call(
            "call-1",
            "get_probe_seed",
            json!({}),
            Some("先取得起始数字"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| messages_len(4)(body) && replayed_reasoning("先取得起始数字")(body),
        assistant_tool_call(
            "call-2",
            "multiply_probe_seed",
            json!({"seed": 7, "factor": 2}),
            Some("继续乘法"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| messages_len(6)(body) && replayed_reasoning("继续乘法")(body),
        assistant_tool_call(
            "call-3",
            "submit_probe_result",
            json!({"submission": {"a": 7, "b": 14, "total": 21, "note": "terminal tool"}}),
            Some("通过终局工具提交"),
        ),
    )
    .await;

    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("traces/agent-check.jsonl");
    let mut trace = Trace::new(Some(trace_path.clone()));
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace)
            .await
            .expect("probe run should be accepted");

    assert_eq!(outcome.final_output, "accepted");
    assert_eq!(outcome.submission.total, 21);
    assert_eq!(outcome.submission.note, "terminal tool");
    assert_eq!(ctx.submission.as_ref().map(|s| s.total), Some(21));
    assert_eq!(
        trace.stats.tool_names,
        [
            "get_probe_seed",
            "multiply_probe_seed",
            "submit_probe_result"
        ]
    );
    assert_eq!(trace.stats.llm_calls, 3);
    assert_eq!(trace.stats.total_tokens, 45);

    // 逐字节回放断言：第三轮请求里应逐字携带前两轮的 reasoning_content。
    let requests = server.received_requests().await.expect("captured requests");
    assert_eq!(requests.len(), 3);
    let last: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(last["max_tokens"], 131_072);
    assert!(last.get("response_format").is_none());
    assert!(replayed_reasoning("先取得起始数字")(&last));
    assert!(replayed_reasoning("继续乘法")(&last));

    // R5 trace：每个事件必带 time/event；tool_start/tool_end 必带工具名+tool_call_id；
    // llm_end/tool_end 必带显式 elapsed_ms（S0 漏埋教训钉死）；禁写 reasoning 内容。
    let trace_text = std::fs::read_to_string(&trace_path).expect("trace file");
    let events: Vec<Value> = trace_text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        events.len() >= 9,
        "llm×3 + tool×3 + 主轮 > 9 行: {}",
        events.len()
    );
    for event in &events {
        assert!(event["time"].is_string(), "{event}");
        assert!(event["event"].is_string(), "{event}");
    }
    for kind in ["llm_start", "llm_end", "tool_start", "tool_end"] {
        assert!(
            events.iter().any(|e| e["event"] == kind),
            "缺少 {kind} 事件"
        );
    }
    for tool_start in events.iter().filter(|e| e["event"] == "tool_start") {
        assert!(
            tool_start["tool"].is_string(),
            "tool 名必须非空: {tool_start}"
        );
        assert!(tool_start["tool_call_id"].is_string(), "{tool_start}");
    }
    for llm_end in events.iter().filter(|e| e["event"] == "llm_end") {
        assert!(llm_end["elapsed_ms"].is_number(), "显式耗时必填: {llm_end}");
    }
    assert!(
        !trace_text.contains("先取得起始数字"),
        "reasoning 内容禁止落 trace"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_text_triggers_forced_terminal_resubmission() {
    let server = MockServer::start().await;
    // turn 1：模型直接输出普通文本（无 tool_calls）。
    mount_turn(
        &server,
        messages_len(2),
        assistant_text("这是草稿，不是有效提交"),
    )
    .await;
    // forced 续跑：tool_choice 具名强制终局，tools 收窄为仅终局，历史保留草稿。
    mount_turn(
        &server,
        |body: &Value| {
            let forced_ok =
                body["tool_choice"]["function"]["name"].as_str() == Some("submit_probe_result");
            let draft_kept = body["messages"].as_array().is_some_and(|msgs| {
                msgs.iter().any(|m| {
                    m["role"].as_str() == Some("assistant")
                        && m["content"].as_str() == Some("这是草稿，不是有效提交")
                })
            });
            let only_terminal = body["tools"].as_array().is_some_and(|tools| {
                tools.len() == 1
                    && tools[0]["function"]["name"].as_str() == Some("submit_probe_result")
            });
            let has_urged_user = body["messages"].as_array().is_some_and(|msgs| {
                msgs.iter().any(|m| {
                    m["role"].as_str() == Some("user")
                        && m["content"]
                            .as_str()
                            .is_some_and(|c| c.contains("上一轮以普通文本结束"))
                })
            });
            forced_ok && draft_kept && only_terminal && has_urged_user
        },
        assistant_tool_call(
            "call-forced",
            "submit_probe_result",
            json!({"submission": {"a": 7, "b": 14, "total": 21}}),
            None,
        ),
    )
    .await;

    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace)
            .await
            .expect("forced resubmission should accept");

    assert_eq!(outcome.submission.total, 21);
    let requests = server.received_requests().await.expect("captured requests");
    assert_eq!(requests.len(), 2, "普通文本后必须发起 forced 续跑");
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_reject_then_correct_resubmission() {
    let server = MockServer::start().await;
    // turn 1：直接提交错误值 → 校验台拒绝，进入下一轮修正。
    mount_turn(
        &server,
        messages_len(2),
        assistant_tool_call(
            "call-bad",
            "submit_probe_result",
            json!({"submission": {"a": 1, "b": 1, "total": 2}}),
            None,
        ),
    )
    .await;
    mount_turn(
        &server,
        messages_len(4),
        assistant_tool_call(
            "call-good",
            "submit_probe_result",
            json!({"submission": {"a": 7, "b": 14, "total": 21}}),
            None,
        ),
    )
    .await;

    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace)
            .await
            .expect("校正后应被接受");

    assert_eq!(outcome.submission.total, 21);
    let requests = server.received_requests().await.expect("captured");
    assert_eq!(requests.len(), 2);
    // 第二轮历史里应带有第一次被拒绝的工具结果（含 accepted:false）。
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let tool_msgs: Vec<&Value> = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"].as_str() == Some("tool"))
        .collect();
    assert_eq!(tool_msgs.len(), 1);
    assert!(
        tool_msgs[0]["content"]
            .as_str()
            .unwrap()
            .contains("\"accepted\":false")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_rounds_without_accept_exhausts_with_named_error() {
    let server = MockServer::start().await;
    // 两轮都输出普通文本：主轮 + forced 续跑均以文本结束 → NoTerminal → Exhausted。
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(assistant_text("只是闲聊")))
        .expect(2)
        .mount(&server)
        .await;

    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let err = run_toolcall_agent::<_, ProbeResult>(
        &runtime,
        &mut spec,
        plan("开始", 8),
        &mut ctx,
        &mut trace,
    )
    .await
    .expect_err("两轮均无终局提交必须失败");

    let text = err.to_string();
    assert!(text.contains("ended without accepted"), "{text}");
    assert!(text.contains("failed after 1 attempts"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn max_turns_exceeded_fails_attempt() {
    let server = MockServer::start().await;
    // 模型永远只调用调查工具，永不提交 → 第 3 轮越过 max_turns=2。
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_tool_call(
                "call-loop",
                "get_probe_seed",
                json!({}),
                Some("继续探索"),
            )),
        )
        .expect(2)
        .mount(&server)
        .await;

    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let err = run_toolcall_agent::<_, ProbeResult>(
        &runtime,
        &mut spec,
        plan("开始", 2),
        &mut ctx,
        &mut trace,
    )
    .await
    .expect_err("超过 max_turns 必须失败");
    assert!(err.to_string().contains("max turns"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_disabled_strips_reasoning_content() {
    let server = MockServer::start().await;
    mount_turn(
        &server,
        messages_len(2),
        assistant_tool_call("call-1", "get_probe_seed", json!({}), Some("保密推理")),
    )
    .await;
    // replay 关闭 ⇒ 第二轮 assistant 历史里不得携带 reasoning_content 键。
    mount_turn(
        &server,
        |body: &Value| messages_len(4)(body) && no_reasoning_replayed()(body),
        assistant_tool_call(
            "call-3",
            "multiply_probe_seed",
            json!({"seed": 7, "factor": 2}),
            Some("第二段推理"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| messages_len(6)(body) && no_reasoning_replayed()(body),
        assistant_tool_call(
            "call-3b",
            "submit_probe_result",
            json!({"submission": {"a": 7, "b": 14, "total": 21}}),
            None,
        ),
    )
    .await;

    let server_uri = server.uri();
    let runtime =
        AgentRuntime::for_test(&server_uri, "custom-reasoning-model", 131_072, true, false);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace)
            .await
            .expect("replay 关闭同样应完成终局");
    assert_eq!(outcome.submission.total, 21);
}

#[test]
fn draft_truncation_is_char_counted() {
    let draft: String = "字".repeat(DRAFT_TRUNCATE_CHARS + 50);
    assert_eq!(
        truncate_chars(&draft, DRAFT_TRUNCATE_CHARS).chars().count(),
        DRAFT_TRUNCATE_CHARS
    );
}

/// 安全 M1：LLM 传输错误落 trace/终态前脱敏——key 片段与响应体原文永不入档。
#[tokio::test(flavor = "multi_thread")]
async fn transport_errors_are_redacted_before_trace() {
    // A) 401 回吐 key 片段 → 脱敏
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "Incorrect API key provided: sk-abcd1234efgh5678",
                      "type": "invalid_request_error", "code": "invalid_api_key"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let runtime = AgentRuntime::for_test(&server.uri(), "m", 131_072, true, true);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let trace_path = tempfile::tempdir().unwrap().path().join("trace.jsonl");
    let mut trace = Trace::new(Some(trace_path.clone()));
    let err = run_toolcall_agent::<ProbeContext, ProbeResult>(
        &runtime,
        &mut spec,
        plan("开始", 4),
        &mut ctx,
        &mut trace,
    )
    .await
    .expect_err("401 必须失败");
    let err_text = err.to_string();
    assert!(err_text.contains("sk-***"), "{err_text}");
    assert!(!err_text.contains("abcd1234"), "{err_text}");
    drop(trace);
    let trace_text = std::fs::read_to_string(&trace_path).unwrap();
    assert!(!trace_text.contains("abcd1234"), "{trace_text}");

    // B) 200 坏形状（choices 非数组）→ body 脱敏，正文不进 trace
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-x", "object": "chat.completion", "created": 1,
            "model": "m", "choices": 42,
            "leaked_reasoning_content": "隐藏思考原文不得入档"
        })))
        .expect(1) // JSONDeserialize 属非瞬时类 → 单次即败
        .mount(&server)
        .await;
    let runtime = AgentRuntime::for_test(&server.uri(), "m", 131_072, true, true);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let trace_path = tempfile::tempdir().unwrap().path().join("trace.jsonl");
    let mut trace = Trace::new(Some(trace_path.clone()));
    let err = run_toolcall_agent::<ProbeContext, ProbeResult>(
        &runtime,
        &mut spec,
        plan("开始", 4),
        &mut ctx,
        &mut trace,
    )
    .await
    .expect_err("坏形状必须失败");
    assert!(err.to_string().contains("redacted"), "{err}");
    drop(trace);
    let trace_text = std::fs::read_to_string(&trace_path).unwrap();
    assert!(!trace_text.contains("隐藏思考"), "{trace_text}");
}
