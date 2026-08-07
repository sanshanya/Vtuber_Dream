//! M6-A YAML_TEMPLATE 共享化：5 份测试复本单一真源（挂账消化 4）。
//!
//! 参数面经逐文件 diff 实测：project/name、model、cookie、api_key、base_url、
//! 文件头注释共六处可变（M6-A 共享化批登记）；其余键值全同。
//! 不建通用测试框架——仅这一份 template 生成函数（AGENTS.md 代码简化规则）。

/// 单一真源模板（v6 基准，键序与 config.example.yaml 同构最小集）。
/// 占位符：`OUTPUT_DIR` 由调用方替换成临时目录（与历史复本一致）。
#[rustfmt::skip]
pub fn yaml_template(
    header_comment: Option<&str>,
    project: &str,
    cookie: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> String {
    // 历史复本以 leading \n 开头（`r#"` 紧跟换行），注释行紧贴其后、
    // 与 version 之间无空行——保持字节级同形，注释保留钉等现状断言零改动。
    let mut text = format!(
        r#"version: 6
project:
  name: {project}
  output_dir: OUTPUT_DIR
bilibili:
  room_id: "983"
  streamer_uid: "9001"
  cookie: "{cookie}"
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
  base_url: {base_url}
  api_key: {api_key}
  model: {model}
  reasoning:
    enabled: false
  agent:
    run_retries: 0
  max_output_tokens: 131072
report:
  title: t
"#,
    );
    text.insert_str(
        0,
        &header_comment
            .map(|line| format!("\n{line}\n"))
            .unwrap_or("\n".to_string()),
    );
    text
}

#[cfg(test)]
mod tests {
    /// 自钉：默认参数组合必须生成与历史复本字节同形的关键行。
    #[test]
    fn template_shape_pin() {
        let text = super::yaml_template(
            None,
            "m5b-runs",
            "SESSDATA=test",
            "test-key",
            "http://127.0.0.1:9/v1",
            "m5b-runs",
        );
        assert!(text.starts_with("\nversion: 6\n"), "{text}");
        assert!(text.contains("cookie: \"SESSDATA=test\"\n"), "{text}");
        assert!(text.contains("base_url: http://127.0.0.1:9/v1\n"), "{text}");
        assert!(text.contains("OUTPUT_DIR"), "{text}");
        let with_comment =
            super::yaml_template(Some("# 注释行"), "p", "SESSDATA=test", "k", "http://x", "m");
        assert!(with_comment.starts_with("\n# 注释行\n"), "{with_comment}");
    }
}
