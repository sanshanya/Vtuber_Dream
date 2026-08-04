//! M5-A T-2：`agent-check` 子命令的 env 门钉（r7-P5 消化）。
//!
//! 钉面：
//! 1. 未设 VTD_AGENT_CHECK → 拒绝（exit 2）且**先于 config 读取**（给不存在的路径仍报门）。
//! 2. 值非 "1" → 同样拒绝（只认显式字面，AGENTS.md 质量门禁·真实端点 opt-in）。
//! 3. VTD_AGENT_CHECK=1 → 过门走正常路径（坏配置落入 `error: {load_error}` 臂）。
//! 4. wiremock 三回合剧本 + 过门 → 全链 PASS 摘要（非真实端点的协议面钉）。

use std::path::PathBuf;
use std::process::Command;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn run(args: &[&str], cwd: &PathBuf, gate: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_live-audience"));
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("VTD_AGENT_CHECK");
    if let Some(value) = gate {
        cmd.env("VTD_AGENT_CHECK", value);
    }
    cmd.output().expect("spawn live-audience")
}

#[test]
fn agent_check_without_env_refused_before_config_read() {
    let root = std::env::temp_dir();
    // 给不存在的配置路径：若实现先读配置，报错文案会是 load 错误而非门指引。
    let output = run(
        &["agent-check", "-c", "definitely-missing-config.yaml"],
        &root,
        None,
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("VTD_AGENT_CHECK"), "stderr={stderr}");
    assert!(
        !stderr.contains("definitely-missing-config"),
        "stderr={stderr}"
    );
}

#[test]
fn agent_check_gate_rejects_non_one_values() {
    let root = std::env::temp_dir();
    for value in ["yes", "true", ""] {
        let output = run(&["agent-check"], &root, Some(value));
        assert_eq!(output.status.code(), Some(2), "value={value:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("VTD_AGENT_CHECK"),
            "value={value:?} stderr={stderr}"
        );
    }
}

#[test]
fn agent_check_with_gate_loads_config_and_reports_error() {
    let root = std::env::temp_dir();
    let output = run(
        &["agent-check", "-c", "definitely-missing-config.yaml"],
        &root,
        Some("1"),
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // 过门后落入 load_config 的错误臂（r6 F-3 `error: {exc}` 形态）。
    assert!(stderr.starts_with("error: "), "stderr={stderr}");
    assert!(!stderr.contains("VTD_AGENT_CHECK"), "stderr={stderr}");
}

// ---------------------------------------------------------------------------
// wiremock 三回合剧本：过门后 CLI 全链（load_config → AgentRuntime::from_ai_config
// → run_toolcall_agent 探针 → PASS 摘要 stdout）。
// ---------------------------------------------------------------------------

/// 保守版 assistant_tool_call（live-core tests/common 同形；reasoning 关 → 不回放）。
fn assistant_tool_call(id: &str, name: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": format!("chatcmpl-{id}"),
        "object": "chat.completion",
        "created": 1_700_000_000,
        "model": "probe-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
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

fn yaml(base_url: &str, output_dir: &str) -> String {
    format!(
        r#"
version: 6
project:
  name: agent-check-cli
  output_dir: "{output_dir}"
bilibili:
  room_id: "1"
  streamer_uid: "0"
  cookie: "SESSDATA=test"
  additional_viewer_ids: []
collection:
  max_guards: 50
  per_viewer_request_budget: 12
  followings_limit: 50
  recent_videos: 10
  recent_dynamics: 30
  favorite_folders: 3
  favorite_items_per_folder: 30
  bangumi_limit: 30
  games_limit: 30
  max_video_metadata_items: 120
  request_delay_seconds: 0
  timeout_seconds: 5
perception:
  peer_discovery:
    candidate_limit: 20
    recent_videos: 8
    recent_dynamics: 8
    max_formal_peers: 8
ai:
  api: chat_completions
  base_url: "{base_url}"
  api_key: test
  model: probe-model
  reasoning:
    enabled: false
  agent:
    max_turns: 4
    run_retries: 0
  max_output_tokens: 131072
report:
  title: t
"#
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_check_pass_against_probe_script() {
    let server = MockServer::start().await;
    // 回合门 = 消息条数：首轮 system+user(2)，随后每轮各 +2（assistant + tool result）。
    // 同 path 叠 mock 先挂优先，2→4→6 序列天然互斥。
    let turns: [(usize, serde_json::Value); 3] = [
        (
            2,
            assistant_tool_call("call-1", "get_probe_seed", json!({})),
        ),
        (
            4,
            assistant_tool_call(
                "call-2",
                "multiply_probe_seed",
                json!({"seed": 7, "factor": 2}),
            ),
        ),
        (
            6,
            // 终局参数必须包 submission：{"submission": {...}}（make_terminal_tool 契约）。
            assistant_tool_call(
                "call-3",
                "submit_probe_result",
                json!({"submission": {"a": 7, "b": 14, "total": 21}}),
            ),
        ),
    ];
    for (len, response) in turns {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(move |req: &Request| {
                serde_json::from_slice::<serde_json::Value>(&req.body)
                    .ok()
                    .and_then(|body| body["messages"].as_array().map(|m| m.len() == len))
                    .unwrap_or(false)
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }
    let root = std::env::temp_dir().join(format!("agent-check-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let config_path = root.join("config.yaml");
    std::fs::write(
        &config_path,
        yaml(
            &server.uri(),
            &root.join("out").to_string_lossy().replace('\\', "/"),
        ),
    )
    .unwrap();

    let output = run(
        &["agent-check", "-c", config_path.to_str().unwrap()],
        &root,
        Some("1"),
    );
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout 是 JSON");
    // Python run_agent_check_async 返回 dict 的键序/键值 parity。
    assert_eq!(payload["status"], "PASS");
    assert_eq!(payload["api"], "chat_completions");
    assert_eq!(payload["model"], "probe-model");
    assert_eq!(payload["reasoning_enabled"], false);
    assert_eq!(payload["output_protocol"], "tool_call_only");
    assert_eq!(payload["terminal_tool"], "submit_probe_result");
    assert_eq!(payload["ordinary_text_final"], false);
    assert_eq!(payload["llm_calls"], 3);
    assert_eq!(payload["tool_calls"], 3);
    assert_eq!(
        payload["tool_sequence"],
        json!([
            "get_probe_seed",
            "multiply_probe_seed",
            "submit_probe_result"
        ])
    );
    assert_eq!(payload["output"]["a"], 7);
    assert_eq!(payload["output"]["b"], 14);
    assert_eq!(payload["output"]["total"], 21);
}
