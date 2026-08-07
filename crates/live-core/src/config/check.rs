//! 自 config.rs 头注条款拆出的校验/归一化面。

use serde_json::{Map, Value};

use super::{Config, ConfigError};

/// Python `cookie_names`：Cookie 字符串中的键集合。
pub(crate) fn cookie_names(cookie: &str) -> std::collections::BTreeSet<String> {
    cookie
        .split(';')
        .filter_map(|part| {
            let part = part.trim();
            let (key, _) = part.split_once('=')?;
            if key.is_empty() {
                None
            } else {
                Some(key.to_string())
            }
        })
        .collect()
}

pub(crate) fn collection_issues(config: &Config) -> Vec<String> {
    let mut issues = Vec::new();
    if config.bilibili.room_id.is_empty() {
        issues.push("bilibili.room_id is empty".to_string());
    }
    if config.bilibili.streamer_uid.is_empty() {
        issues.push("bilibili.streamer_uid is empty".to_string());
    }
    if config.bilibili.cookie.is_empty() {
        issues.push("bilibili.cookie is empty".to_string());
    } else if !cookie_names(&config.bilibili.cookie).contains("SESSDATA") {
        issues.push("bilibili.cookie is missing SESSDATA".to_string());
    }
    issues
}

pub(crate) fn ai_issues(config: &Config) -> Vec<String> {
    let mut issues = Vec::new();
    if config.ai.base_url.is_empty() {
        issues.push("ai.base_url is empty".to_string());
    } else {
        let url = &config.ai.base_url;
        let scheme_ok = url.starts_with("http://") || url.starts_with("https://");
        let host = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("");
        if !scheme_ok || host.is_empty() {
            issues.push("ai.base_url must be an HTTP(S) API root URL".to_string());
        }
        if url.trim_end_matches('/').ends_with("/chat/completions") {
            issues.push(
                "ai.base_url must be the API root, not the /chat/completions endpoint".to_string(),
            );
        }
    }
    if config.ai.api_key.is_empty() {
        issues.push("ai.api_key is empty".to_string());
    }
    if config.ai.model.is_empty() {
        issues.push("ai.model is empty".to_string());
    }
    issues
}

pub fn validate_for_collection(config: &Config) -> Result<(), ConfigError> {
    let issues = collection_issues(config);
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::new(issues.join("; ")))
    }
}

pub fn validate_for_ai(config: &Config) -> Result<(), ConfigError> {
    let issues = ai_issues(config);
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::new(issues.join("; ")))
    }
}

/// 归一化快照：仅非秘密字段。与 `tests-fixtures/config/example.normalized.json`
/// 的关系是**子集相等**（fixture 记录的每个键值必须与本输出一致）。
pub fn normalized_json(config: &Config) -> Value {
    let mut root = Map::new();
    root.insert(
        "project_name".to_string(),
        Value::String(config.project_name.clone()),
    );

    let mut bilibili = Map::new();
    bilibili.insert(
        "room_id".to_string(),
        Value::String(config.bilibili.room_id.clone()),
    );
    bilibili.insert(
        "streamer_uid".to_string(),
        Value::String(config.bilibili.streamer_uid.clone()),
    );
    bilibili.insert(
        "additional_viewer_ids".to_string(),
        Value::Array(
            config
                .bilibili
                .additional_viewer_ids
                .iter()
                .map(|id| Value::String(id.clone()))
                .collect(),
        ),
    );
    root.insert("bilibili".to_string(), Value::Object(bilibili));

    let mut collection = Map::new();
    collection.insert(
        "per_viewer_request_budget".to_string(),
        Value::from(config.collection.per_viewer_request_budget),
    );
    collection.insert(
        "max_video_metadata_items".to_string(),
        Value::from(config.collection.max_video_metadata_items),
    );
    root.insert("collection".to_string(), Value::Object(collection));

    let mut ai = Map::new();
    ai.insert("api".to_string(), Value::String(config.ai.api.clone()));
    ai.insert(
        "base_url".to_string(),
        Value::String(config.ai.base_url.clone()),
    );
    ai.insert("model".to_string(), Value::String(config.ai.model.clone()));
    let mut reasoning = Map::new();
    reasoning.insert(
        "enabled".to_string(),
        Value::Bool(config.ai.reasoning.enabled),
    );
    reasoning.insert(
        "effort".to_string(),
        Value::String(config.ai.reasoning.effort.clone()),
    );
    reasoning.insert(
        "replay_content".to_string(),
        Value::Bool(config.ai.reasoning.replay_content),
    );
    ai.insert("reasoning".to_string(), Value::Object(reasoning));
    let mut agent = Map::new();
    agent.insert(
        "run_retries".to_string(),
        Value::from(config.ai.agent.run_retries),
    );
    agent.insert(
        "viewer_token_budget".to_string(),
        Value::from(config.ai.agent.viewer_token_budget),
    );
    ai.insert("agent".to_string(), Value::Object(agent));
    ai.insert(
        "max_output_tokens".to_string(),
        Value::from(config.ai.max_output_tokens),
    );
    root.insert("ai".to_string(), Value::Object(ai));

    let mut peer = Map::new();
    peer.insert(
        "candidate_limit".to_string(),
        Value::from(config.perception.peer.candidate_limit),
    );
    peer.insert(
        "recent_videos".to_string(),
        Value::from(config.perception.peer.recent_videos),
    );
    peer.insert(
        "recent_dynamics".to_string(),
        Value::from(config.perception.peer.recent_dynamics),
    );
    peer.insert(
        "max_formal_peers".to_string(),
        Value::from(config.perception.peer.max_formal_peers),
    );
    let mut perception = Map::new();
    perception.insert("peer".to_string(), Value::Object(peer));
    root.insert("perception".to_string(), Value::Object(perception));

    Value::Object(root)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::{load_config, testkit::*};

    #[test]
    fn normalized_parity_with_fixture() {
        let file = write_temp(EXAMPLE_YAML);
        let config = load_config(file.path()).unwrap();
        let normalized = normalized_json(&config);
        let fixture: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests-fixtures/config/example.normalized.json"),
            )
            .unwrap(),
        )
        .unwrap();
        // 子集相等：fixture 的每个键值都必须出现在归一化输出中。
        assert_subset(&fixture, &normalized, "");
    }

    fn assert_subset(fixture: &Value, actual: &Value, path: &str) {
        match fixture {
            Value::Object(map) => {
                for (key, expected) in map {
                    let child = actual
                        .get(key)
                        .unwrap_or_else(|| panic!("missing key {path}/{key}"));
                    assert_subset(expected, child, &format!("{path}/{key}"));
                }
            }
            other => assert_eq!(other, actual, "mismatch at {path}"),
        }
    }

    #[test]
    fn ai_issues_flag_endpoint_form() {
        let file = write_temp(EXAMPLE_YAML);
        let mut config = load_config(file.path()).unwrap();
        assert_eq!(ai_issues(&config), Vec::<String>::new());
        config.ai.base_url = "https://example.test/v1/chat/completions".to_string();
        let issues = ai_issues(&config);
        assert!(issues.iter().any(|issue| issue.contains("API root")));
    }

    #[test]
    fn collection_requires_sessdata() {
        let file = write_temp(EXAMPLE_YAML);
        let mut config = load_config(file.path()).unwrap();
        assert_eq!(collection_issues(&config), Vec::<String>::new());
        config.bilibili.cookie = "DedeUserID=1".to_string();
        let issues = collection_issues(&config);
        assert!(issues.iter().any(|issue| issue.contains("SESSDATA")));
    }
}
