//! M3-D 真实端点 smoke（显式 opt-in）：不设 `VTD_DEEPSEEK_KEY` 即跳过，绝不成为
//! 普通单元测试依赖（AGENTS.md 质量门禁·真实端点条款）。
//!
//! 剧本 = Python agent-check 同型：probe 三工具（seed→multiply→terminal submit）。
//! 断言面：终局接受 a=7/b=14/total=21。reasoning 回放的请求体字段级钉在 wiremock
//! 剧本侧（agent_runtime/agent_golden）；真实网关无法捕获请求体，此处的确认方式是
//! 「reasoning 开启 + replay 开启 + 多轮通关」端到端达成（kickoff ①的实战面）。

use live_core::agent::probe::{ProbeContext, probe_spec};
use live_core::agent::runtime::{AgentRuntime, AttemptPlan, Trace, run_toolcall_agent};
use live_core::config::{AgentRuntimeConfig, AiConfig, ReasoningConfig};
use live_core::models::ProbeResult;

/// smoke 预算秒数（ji opt-in 任务不烧满 CI 默认超时；timeout_seconds=900 是单项 HTTP，
/// attempts×turns 组合下病理上限远超任何 job 容忍，测试体自加 deadline）。
const SMOKE_BUDGET_SECONDS: u64 = 600;

fn ai_config(api_key: &str) -> AiConfig {
    AiConfig {
        api: "chat_completions".to_string(),
        base_url: std::env::var("VTD_DEEPSEEK_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com".to_string()),
        api_key: api_key.to_string(),
        model: std::env::var("VTD_DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string()),
        timeout_seconds: 900.0,
        max_output_tokens: 131_072,
        reasoning: ReasoningConfig {
            enabled: true,
            effort: String::new(),
            replay_content: true,
        },
        agent: AgentRuntimeConfig {
            max_turns: 8,
            resume: false,
            local_trace: true,
            run_retries: 1,
            retry_backoff_seconds: 3.0,
            viewer_token_budget: 200_000,
            max_parallel_viewers: 4,
            max_llm_rpm: 0,
        },
        search_results_per_query: 20,
        rules: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deepseek_probe_smoke_opt_in() {
    let Ok(api_key) = std::env::var("VTD_DEEPSEEK_KEY") else {
        eprintln!("skipped: VTD_DEEPSEEK_KEY not set（opt-in 真实端点）");
        return;
    };
    let ai = ai_config(&api_key);
    let runtime = AgentRuntime::from_ai_config(&ai).expect("runtime from config");
    let mut spec = probe_spec();
    let mut ctx = ProbeContext::new(7);
    let tmp = tempfile::tempdir().unwrap();
    let mut trace = Trace::new(Some(tmp.path().join("trace.jsonl")));
    let run = run_toolcall_agent::<ProbeContext, ProbeResult>(
        &runtime,
        &mut spec,
        AttemptPlan {
            label: "deepseek-probe",
            prompt: "开始执行探针。",
            max_turns: ai.agent.max_turns.max(4) as usize,
            retries: ai.agent.run_retries.max(0) as usize,
            backoff_seconds: ai.agent.retry_backoff_seconds,
            token_budget: None,
        },
        &mut ctx,
        &mut trace,
    );
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(SMOKE_BUDGET_SECONDS), run)
        .await
        .expect("smoke 超出预算秒数")
        .expect("deepseek probe accepted");
    assert_eq!(outcome.final_output, "accepted");
    assert_eq!(outcome.submission.a, 7);
    assert_eq!(outcome.submission.b, 14);
    assert_eq!(outcome.submission.total, 21);
}
