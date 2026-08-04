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
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// wiremock 自定义 matcher：按请求体 JSON 谓词路由（剧本分流器）。
mod common;
use common::*;

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
        token_budget: None,
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
        |body: &Value| {
            messages_len(6)(body)
                && replayed_reasoning("继续乘法")(body)
                && reasoning_attribution()(body)
        },
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
            // 协议 M1：forced 请求的 system 必须是具名强制文案（替换主 instructions）。
            let forced_instructions = body["messages"][0]["role"].as_str() == Some("system")
                && body["messages"][0]["content"].as_str().is_some_and(|c| {
                    c.contains("现在只能调用 submit_probe_result")
                        && c.contains("必须修正所有校验问题并通过该工具提交")
                });
            let forced_ok =
                body["tool_choice"]["function"]["name"].as_str() == Some("submit_probe_result");
            let four_messages = body["messages"].as_array().map(Vec::len) == Some(4);
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
            forced_ok
                && draft_kept
                && only_terminal
                && has_urged_user
                && forced_instructions
                && four_messages
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

// ---------------------------------------------------------------------------
// r1-F1：单 agent token 预算熔断——触顶即时终止，走既有 Fatal/失败通道
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn token_budget_exceeded_terminates_immediately() {
    let server = MockServer::start().await;
    // 每回合 claim 5000 total_tokens；预算 100 → 第一回合即熔断。
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_bill("只是闲聊预算烧穿", 5000)),
        )
        .mount(&server)
        .await;

    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let err = run_toolcall_agent::<_, ProbeResult>(
        &runtime,
        &mut spec,
        AttemptPlan {
            label: "agent-check",
            prompt: "开始",
            max_turns: 8,
            retries: 2, // 熔断不可重试：即便配置 retries 也必须一次终止
            backoff_seconds: 0.0,
            token_budget: Some(100),
        },
        &mut ctx,
        &mut trace,
    )
    .await
    .expect_err("超预算必须触顶终止");
    let text = err.to_string();
    assert!(text.contains("token_budget"), "{text}");
    // 只产生 1 次 LLM 请求：无重试、无 forced 续跑
    let requests = server.received_requests().await.expect("captured");
    assert_eq!(requests.len(), 1, "{requests:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn default_budget_keeps_accepting_flows() {
    let server = MockServer::start().await;
    mount_turn(
        &server,
        messages_len(2),
        assistant_tool_call("call-1", "get_probe_seed", json!({}), Some("取种子")),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| messages_len(4)(body),
        assistant_tool_call(
            "call-2",
            "multiply_probe_seed",
            json!({"seed": 7, "factor": 2}),
            Some("乘二"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| messages_len(6)(body),
        assistant_tool_call(
            "call-3",
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
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> = run_toolcall_agent(
        &runtime,
        &mut spec,
        AttemptPlan {
            label: "agent-check",
            prompt: "开始",
            max_turns: 8,
            retries: 0,
            backoff_seconds: 0.0,
            token_budget: Some(200_000),
        },
        &mut ctx,
        &mut trace,
    )
    .await
    .expect("默认预算 200k 下照常接受");
    assert_eq!(outcome.submission.total, 21);
    assert_eq!(trace.stats.total_tokens, 45);
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

/// 协议 M2：408/409/429/5xx 与 reqwest 瞬态同族内层重试（≤2）——429→200 透明恢复。
#[tokio::test(flavor = "multi_thread")]
async fn http_429_then_200_recovers_within_inner_retry() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |_: &Request| {
            if counter_clone.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).set_body_json(json!({
                    "error": {"message": "rate limited", "type": "rate_limit_error", "code": "rate_limit"}
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(assistant_tool_call(
                    "call-ok",
                    "submit_probe_result",
                    json!({"submission": {"a": 7, "b": 14, "total": 21}}),
                    None,
                ))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace)
            .await
            .expect("429 应被内层瞬时重试恢复");
    assert_eq!(outcome.submission.total, 21);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

/// r1-M4：429 + 裸文本 body → 错误解析为 JSONDeserialize 形态 → 非瞬时 → 单次即败。
/// 钉的是**现行**语义：状态码驱动的内层恢复只在 body 是 OpenAI 错误 JSON 时成立
/// （对照 `http_429_then_200_recovers_within_inner_retry`）。与 Python httpx 状态码
/// 驱动相左是已登记的偏差单（r1-M4，非 M4 引入），本钉防静默漂移。
#[tokio::test(flavor = "multi_thread")]
async fn http_429_bare_text_body_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limit hit"))
        .expect(1) // JSONDeserialize 非瞬时族 → 无内层重试
        .mount(&server)
        .await;
    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let err = run_toolcall_agent::<ProbeContext, ProbeResult>(
        &runtime,
        &mut spec,
        plan("开始", 4),
        &mut ctx,
        &mut trace,
    )
    .await
    .expect_err("429 裸文本 body 按现行语义不可恢复");
    let text = err.to_string();
    assert!(text.contains("chat transport"), "{text}");
    assert!(text.contains("1 attempts"), "{text}");
}

// ---------------------------------------------------------------------------
// Z3/P0-4：三维闸门（限速漏桶 + Retry-After 尊重 + 指数退避确定性缝）
// ---------------------------------------------------------------------------

/// Z3/P0-4：Retry-After 信标被尊重——429 body message 携带 "Retry-After: 1" 时，
/// 重试前实睡 ≥ 该秒数（抖动经 VTD_LIVE_CORE_TEST_JITTER_SEED 钉 0 消噪）。
/// 局限（书面）：async-openai 的 ApiError 不透传响应头，信标只好从 redact 保留的
/// message 段截取；真实网关若仅走 header，本机制降级为纯指数退避。
#[tokio::test(flavor = "multi_thread")]
async fn http_429_retry_after_hint_is_honored() {
    unsafe { std::env::set_var("VTD_LIVE_CORE_TEST_JITTER_SEED", "0") };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |_: &Request| {
            if counter_clone.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "1") // header 面（SDK 不透传，记录用）
                    .set_body_json(json!({
                        "error": {
                            "message": "rate limited. Retry-After: 1 before next attempt",
                            "type": "rate_limit_error",
                            "code": "rate_limit"
                        }
                    }))
            } else {
                ResponseTemplate::new(200).set_body_json(assistant_tool_call(
                    "call-ok",
                    "submit_probe_result",
                    json!({"submission": {"a": 7, "b": 14, "total": 21}}),
                    None,
                ))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let started = std::time::Instant::now();
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace)
            .await
            .expect("Retry-After 提示后应恢复");
    assert_eq!(outcome.submission.total, 21);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert!(
        started.elapsed().as_secs_f64() >= 0.9,
        "Retry-After: 1 未被尊重（耗时 {:?}）",
        started.elapsed()
    );
}

/// Z3/P0-4 实验门核心钉：run 级漏桶 cap=2 req/min（30s/许可）下，3 个并发 agent
/// 的第 3 张许可必落在 ~60s 处——即同一 60 秒滑窗内出队请求数 ≤ 2+1。
/// makespan ∈ [59s, 70s] 同时钉「闸门生效」（≥59s）与「压缩不失真」（≤70s：
/// 若每个等待者各自完整串行睡眠则 ≥120s）。许可即请求（单轮终局 ⇒ 每 agent 恰 1 请求）。
#[tokio::test(flavor = "multi_thread")]
async fn throttle_caps_requests_in_sliding_minute_window() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |_: &Request| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(assistant_tool_call(
                &format!("call-{n}"),
                "submit_probe_result",
                json!({"submission": {"a": 7, "b": 14, "total": 21}}),
                None,
            ))
        })
        .mount(&server)
        .await;
    let runtime = std::sync::Arc::new(test_runtime(&server).with_throttle(std::sync::Arc::new(
        live_core::agent::throttle::Throttle::limited(2),
    )));
    let started = std::time::Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..3 {
        let runtime = runtime.clone();
        set.spawn(async move {
            let mut spec = probe_spec();
            let mut ctx = ProbeContext::new(7);
            let mut trace = Trace::none();
            run_toolcall_agent::<ProbeContext, ProbeResult>(
                &runtime,
                &mut spec,
                plan("开始", 8),
                &mut ctx,
                &mut trace,
            )
            .await
            .expect("限速下 agent 应成功")
            .submission
            .total
        });
    }
    let mut results = Vec::new();
    while let Some(joined) = set.join_next().await {
        results.push(joined.expect("task join"));
    }
    let elapsed = started.elapsed();
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|total| *total == 21));
    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert!(
        elapsed >= std::time::Duration::from_secs(59),
        "3 并发 @2rpm：第 3 许可应在 ~60s，实测 {elapsed:?}——闸门未生效"
    );
    assert!(
        elapsed <= std::time::Duration::from_secs(70),
        "3 并发 @2rpm 应在 ~60s 完成，实测 {elapsed:?}——串行化失真"
    );
}

/// Z3/P0-4 对照臂：Throttle::disabled（config 默认 max_llm_rpm=0）完全不减速。
#[tokio::test(flavor = "multi_thread")]
async fn throttle_disabled_never_waits() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_tool_call(
                "call-ok",
                "submit_probe_result",
                json!({"submission": {"a": 7, "b": 14, "total": 21}}),
                None,
            )),
        )
        .mount(&server)
        .await;
    let runtime = std::sync::Arc::new(test_runtime(&server));
    let started = std::time::Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..3 {
        let runtime = runtime.clone();
        set.spawn(async move {
            let mut spec = probe_spec();
            let mut ctx = ProbeContext::new(7);
            let mut trace = Trace::none();
            run_toolcall_agent::<ProbeContext, ProbeResult>(
                &runtime,
                &mut spec,
                plan("开始", 8),
                &mut ctx,
                &mut trace,
            )
            .await
            .expect("无限速 agent 应成功")
        });
    }
    while set.join_next().await.is_some() {}
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "关闭态不应引入等待，实测 {:?}",
        started.elapsed()
    );
}

/// 协议 m5：forced 期间 dispatch 收窄——非终局名得 SDK 文本 not found 而后终局可成。
#[tokio::test(flavor = "multi_thread")]
async fn forced_dispatch_rejects_non_terminal_with_python_text() {
    let server = MockServer::start().await;
    mount_turn(&server, messages_len(2), assistant_text("草稿")).await;
    // forced turn 1：模型调皮调非终局工具 → 收窄拒
    mount_turn(
        &server,
        |body: &Value| {
            body["tool_choice"]["function"]["name"].as_str() == Some("submit_probe_result")
                && body["messages"].as_array().map(Vec::len) == Some(4)
        },
        assistant_tool_call("call-naughty", "get_probe_seed", json!({}), None),
    )
    .await;
    // forced turn 2：终局接受；请求历史必须包含 not found 工具结果
    mount_turn(
        &server,
        |body: &Value| {
            body["messages"].as_array().is_some_and(|msgs| {
                msgs.iter().any(|m| {
                    m["role"].as_str() == Some("tool")
                        && m["content"]
                            .as_str()
                            .is_some_and(|c| c.contains("Tool 'get_probe_seed' not found."))
                })
            })
        },
        assistant_tool_call(
            "call-forced-2",
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
            .expect("forced 收窄后终局应接受");
    assert_eq!(outcome.submission.total, 21);
}

/// 工程 m2：非法 tool arguments JSON 不执行 handler、回喂可读错误。
#[tokio::test(flavor = "multi_thread")]
async fn invalid_tool_arguments_json_gets_feedback_without_handler() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .and(BodyPred(messages_len(2)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-bad", "object": "chat.completion", "created": 1,
            "model": "custom-reasoning-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": null,
                "tool_calls": [{"id": "call-bad", "type": "function",
                    "function": {"name": "get_probe_seed", "arguments": "not-json"}}]},
                "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_turn(
        &server,
        |body: &Value| {
            body["messages"].as_array().is_some_and(|msgs| {
                msgs.iter().any(|m| {
                    m["role"].as_str() == Some("tool")
                        && m["content"]
                            .as_str()
                            .is_some_and(|c| c.contains("invalid tool arguments"))
                })
            })
        },
        assistant_tool_call(
            "call-fix",
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
            .expect("非法参数反馈后模型可自愈");
    assert_eq!(outcome.submission.total, 21);
    // handler 若被执行，工具结果会是 {"seed":7}，turn1 谓词不匹配 → 404 → 上面 expect 即失败（路由即钉）。
}

#[tokio::test(flavor = "multi_thread")]
async fn transport_5xx_requests_exactly_inner_retry_budget() {
    // 回归钉（async-openai 0.41.3 隐藏 OpenAIRetryLayer 事故）：
    // transport 瞬时错的唯一重试所有者是 chat() 内层（HTTP_EXTRA_ATTEMPTS=2）；
    // 客户端 executor 必须零重试 → retries=0 的 agent floatErr 后总请求数恒为 3。
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "boom"}})),
        )
        .expect(3)
        .mount(&server)
        .await;
    let runtime = test_runtime(&server);
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let outcome: Result<live_core::agent::runtime::RunOutcome<ProbeResult>, _> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace).await;
    let err = outcome.expect_err("恒 500 必须失败").to_string();
    assert!(err.contains("failed after 1 attempts"), "err={err}");
    assert_eq!(trace.stats.llm_calls, 1, "transport 错不烧 turn");
}

#[tokio::test(flavor = "multi_thread")]
async fn from_ai_config_transport_5xx_also_exactly_inner_budget() {
    // 评审 C-4：transport_5xx 钉经 for_test；本钉锁定 from_ai_config 同约束——
    // 防止未来的单侧改回 with_http_client 而测试仍绿。
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "boom"}})),
        )
        .expect(3)
        .mount(&server)
        .await;
    let config = live_core::config::AiConfig {
        api: "chat_completions".to_string(),
        base_url: server.uri(),
        api_key: "test".to_string(),
        model: "m".to_string(),
        timeout_seconds: 5.0,
        max_output_tokens: 1024,
        reasoning: live_core::config::ReasoningConfig {
            enabled: false,
            effort: "high".to_string(),
            replay_content: true,
        },
        agent: live_core::config::AgentRuntimeConfig {
            max_turns: 4,
            resume: false,
            local_trace: false,
            run_retries: 0,
            retry_backoff_seconds: 0.0,
            viewer_token_budget: 200_000,
            max_parallel_viewers: 4,
            max_llm_rpm: 0,
        },
        search_results_per_query: 5,
        rules: vec![],
    };
    let runtime = AgentRuntime::from_ai_config(&config).expect("config ok");
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let outcome: Result<live_core::agent::runtime::RunOutcome<ProbeResult>, _> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace).await;
    assert!(outcome.is_err());
    assert_eq!(trace.stats.llm_calls, 1, "transport 错不烧 turn");
}

/// W2-C1（M6-C② 实战炸点）：工具 handler 中执行真实 blocking HTTP 必须
/// 不炸「Cannot drop a runtime…」——reqwest::blocking 的 debug 构建 shell
/// runtime 自查在 async ctx 下必 panic；修法 = 调度点卸到 scoped std::thread
/// （无 tokio ctx 的干净线程）。此钉在修复前的执行形态 = task panic。
#[tokio::test(flavor = "multi_thread")]
async fn tool_handler_with_blocking_http_survives_async_context() {
    use std::sync::{Arc, Mutex};

    use live_core::agent::runtime::ToolHandler;

    let server = MockServer::start().await;
    mount_turn(
        &server,
        messages_len(2),
        assistant_tool_call("call-b1", "blocking_probe", json!({}), Some("先探活")),
    )
    .await;
    mount_turn(
        &server,
        messages_len(4),
        assistant_tool_call(
            "call-t",
            "submit_probe_result",
            json!({"submission": {"a": 7, "b": 14, "total": 21, "note": ""}}),
            None,
        ),
    )
    .await;

    // 记录器证明 handler 真跑过（visitation + 错误形态落笔）。
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log_capture: Arc<Mutex<Vec<String>>> = log.clone();
    let blocking_probe = live_core::agent::runtime::AgentTool::<ProbeContext> {
        name: "blocking_probe".to_string(),
        description: "向不可达 B 站端点发真实 blocking HTTP——修前 async ctx 执行即 panic。"
            .to_string(),
        parameters: json!({"type": "object", "properties": {}, "required": []}),
        terminal: false,
        handler: {
            let boxed: ToolHandler<ProbeContext> = Box::new(move |_, _| {
                let client = live_core::bilibili::BilibiliClient::with_origin(
                    "http://127.0.0.1:9",
                    "http://127.0.0.1:9",
                    "SESSDATA=test",
                    0.0,
                    5.0,
                );
                let entry = match client.and_then(|mut c| c.nav()) {
                    Ok(_) => "executed:ok".to_string(),
                    Err(err) => format!("executed:error:{err}"),
                };
                log_capture.lock().unwrap().push(entry.clone());
                json!({"probe": entry})
            });
            boxed
        },
    };
    let mut spec = probe_spec();
    spec.tools.insert(0, blocking_probe);
    let runtime = test_runtime(&server);
    let mut ctx = ProbeContext::new(7);
    let mut trace = Trace::none();
    let outcome: live_core::agent::runtime::RunOutcome<ProbeResult> =
        run_toolcall_agent(&runtime, &mut spec, plan("开始", 8), &mut ctx, &mut trace)
            .await
            .expect("blocking 工具不得在 async ctx 炸锅");
    assert_eq!(outcome.final_output, "accepted");
    let entries = log.lock().unwrap().clone();
    assert_eq!(entries.len(), 1, "handler 必须恰好跑一次：{entries:?}");
    assert!(
        entries[0].starts_with("executed:error:"),
        "不可达端点应回错误形态而非 panic：{entries:?}"
    );
}
