//! M4-D CLI 冒烟：CARGO_BIN_EXE_live-audience 实跑 `demo -c <yaml>`：
//! exit 0 + stdout 可解析 + 产物面 + `--output` 与用法错误两负例。
use std::path::PathBuf;
use std::process::Command;

// 全键版（config.rs EXAMPLE_YAML 同族）：integer()/required 校验对 collection/ai 各键
// 无宽容缺省，冒烟用全键最省事。
const YAML: &str = r#"
version: 6
project:
  name: m4d-cli
  output_dir: OUTPUT_DIR
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
  base_url: http://127.0.0.1:9/v1
  api_key: test
  model: m4d-cli
  reasoning:
    enabled: false
  agent:
    max_turns: 4
    run_retries: 0
  max_output_tokens: 131072
report:
  title: t
"#;

fn run(args: &[&str], cwd: &PathBuf) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_live-audience"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn live-audience")
}

#[test]
fn demo_end_to_end_and_default_demo_namespace() {
    let root = std::env::temp_dir().join(format!("m4d-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let yaml = YAML.replace(
        "OUTPUT_DIR",
        &root.join("runs").to_string_lossy().replace('\\', "/"),
    );
    let config_path = root.join("config.yaml");
    std::fs::write(&config_path, yaml).unwrap();

    let output = run(&["demo", "-c", config_path.to_str().unwrap()], &root);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout 是 JSON");
    assert_eq!(payload["status"], "complete");
    // D-6：默认输出根 = output_dir 同级 _demo
    let demo_root = root.join("_demo");
    assert_eq!(
        payload["output_dir"].as_str().unwrap().replace('\\', "/"),
        demo_root.to_string_lossy().replace('\\', "/")
    );
    for rel in [
        "collection.json",
        "viewers/demo-1.json",
        "ai/situation.json",
        "ai/state.json",
        "graph/perception.sqlite3",
    ] {
        assert!(demo_root.join(rel).exists(), "缺 {rel}");
    }
    assert!(!demo_root.join("peers").exists(), "D-5：peers 不产");

    // --output 显式目录
    let explicit = root.join("custom-demo");
    let output = run(
        &[
            "demo",
            "-c",
            config_path.to_str().unwrap(),
            "--output",
            explicit.to_str().unwrap(),
        ],
        &root,
    );
    assert!(output.status.success());
    assert!(explicit.join("collection.json").exists());
}

#[test]
fn usage_error_exit_code_2() {
    let root = std::env::temp_dir();
    let output = run(&["frobnicate"], &root);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("未知命令"));
    let output = run(&["demo", "--config"], &root);
    assert_eq!(output.status.code(), Some(2));
}
