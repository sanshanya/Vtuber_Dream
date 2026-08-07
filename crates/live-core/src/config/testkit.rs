//! 测试共享件：加载基线 YAML 与临时文件写入（自 config.rs 内联 tests 拆出）。
//!
//! 门控由 config.rs 根的 `#[cfg(test)] mod testkit;` 完成，本文件内不再写 cfg。

pub(crate) const EXAMPLE_YAML: &str = r#"
version: 6
project:
  name: audience-pilot
  output_dir: runs/first
bilibili:
  room_id: "填写直播间ID"
  streamer_uid: "填写主播UID"
  cookie: "SESSDATA=replace-me; DedeUserID=replace-me"
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
  request_delay_seconds: 1.0
  timeout_seconds: 30
perception:
  peer_discovery:
    candidate_limit: 20
    recent_videos: 8
    recent_dynamics: 8
    max_formal_peers: 8
ai:
  api: chat_completions
  base_url: https://example.test/v1
  api_key: replace-me
  model: custom-reasoning-model
  reasoning:
    enabled: true
    effort: high
    replay_content: true
  agent:
    run_retries: 2
  max_output_tokens: 131072
report:
  title: 直播观众态势感知
"#;

pub(crate) fn write_temp(content: &str) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), content).unwrap();
    file
}
