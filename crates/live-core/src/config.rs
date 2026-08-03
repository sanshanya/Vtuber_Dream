//! 运行配置：serde_yml 装载 + 校验 + 归一化快照（移植 Python `config.py`）。
//!
//! 归一化快照只含**非秘密**字段（cookie/api_key 永不进入），与
//! `tests-fixtures/config/example.normalized.json` 对账（子集相等语义）。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[derive(Debug, Clone)]
pub struct BilibiliConfig {
    pub room_id: String,
    pub streamer_uid: String,
    pub cookie: String,
    pub additional_viewer_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CollectionConfig {
    pub max_guards: i64,
    pub per_viewer_request_budget: i64,
    pub followings_limit: i64,
    pub recent_videos: i64,
    pub recent_dynamics: i64,
    pub favorite_folders: i64,
    pub favorite_items_per_folder: i64,
    pub bangumi_limit: i64,
    pub games_limit: i64,
    pub max_video_metadata_items: i64,
    pub request_delay_seconds: f64,
    pub timeout_seconds: f64,
    /// 评论区浅存在请求预算（design M2-B2c：3~8 请求独立记账；0 关闭该采集点）。
    pub room_comment_request_budget: i64,
    /// 回放弹幕拉取的场数上限（design M2：默认 20 场）。
    pub live_replay_danmaku_limit: i64,
    /// M4.x：单轮 collect 的 leads 消费尝试预算（attempt 计次；0=休眠=默认人工
    /// 审批文化，薄切条款；Rust-only 能力，无 Python 对照键）。
    pub lead_fetch_budget_per_run: i64,
}

#[derive(Debug, Clone)]
pub struct PeerDiscoveryConfig {
    pub candidate_limit: i64,
    pub recent_videos: i64,
    pub recent_dynamics: i64,
    pub max_formal_peers: i64,
}

#[derive(Debug, Clone)]
pub struct PerceptionConfig {
    pub max_evidence_per_viewer: i64,
    pub preserve_raw_snapshots: bool,
    pub platform_hot_search_limit: i64,
    pub minimum_community_size: i64,
    pub peer: PeerDiscoveryConfig,
}

pub const ALLOWED_EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone)]
pub struct ReasoningConfig {
    pub enabled: bool,
    pub effort: String,
    pub replay_content: bool,
}

#[derive(Debug, Clone)]
pub struct AgentRuntimeConfig {
    pub max_turns: i64,
    pub resume: bool,
    pub local_trace: bool,
    pub run_retries: i64,
    pub retry_backoff_seconds: f64,
}

pub const ALLOWED_APIS: [&str; 2] = ["chat_completions", "responses"];

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub api: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_seconds: f64,
    pub max_output_tokens: i64,
    pub reasoning: ReasoningConfig,
    pub agent: AgentRuntimeConfig,
    pub search_results_per_query: i64,
    pub rules: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub source: PathBuf,
    pub project_name: String,
    pub output_dir: PathBuf,
    pub bilibili: BilibiliConfig,
    pub collection: CollectionConfig,
    pub perception: PerceptionConfig,
    pub ai: AiConfig,
    pub report_title: String,
}

static EMPTY_MAPPING: std::sync::LazyLock<Value> =
    std::sync::LazyLock::new(|| Value::Object(Map::new()));

fn empty_mapping() -> &'static Value {
    &EMPTY_MAPPING
}

fn mapping<'a>(
    parent: &'a Value,
    key: &str,
    default: Option<&'static Value>,
) -> Result<&'a Value, ConfigError> {
    let value = parent.get(key).or(default).unwrap_or(&Value::Null);
    if value.is_object() {
        Ok(value)
    } else {
        Err(ConfigError::new(format!("'{key}' must be a mapping")))
    }
}

fn integer(
    mapping: &Value,
    key: &str,
    minimum: i64,
    default: Option<i64>,
) -> Result<i64, ConfigError> {
    let raw = mapping.get(key);
    let value: Option<i64> = match raw {
        None => default,
        // Python int(raw)：浮点截断、数字字符串可解析；bool 在这里视为非法。
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        Some(_) => None,
    };
    let Some(value) = value else {
        return Err(ConfigError::new(format!("'{key}' must be an integer")));
    };
    if value < minimum {
        return Err(ConfigError::new(format!("'{key}' must be >= {minimum}")));
    }
    Ok(value)
}

fn number(
    mapping: &Value,
    key: &str,
    minimum: f64,
    default: Option<f64>,
) -> Result<f64, ConfigError> {
    let raw = mapping.get(key);
    let value: Option<f64> = match raw {
        None => default,
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(_) => None,
    };
    let Some(value) = value else {
        return Err(ConfigError::new(format!("'{key}' must be a number")));
    };
    if value < minimum {
        return Err(ConfigError::new(format!("'{key}' must be >= {minimum}")));
    }
    Ok(value)
}

fn boolean(mapping: &Value, key: &str, default: bool) -> Result<bool, ConfigError> {
    match mapping.get(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(ConfigError::new(format!("'{key}' must be true or false"))),
    }
}

fn string(mapping: &Value, key: &str, default: &str) -> String {
    match mapping.get(key) {
        Some(value) => py_str_config(value).trim().to_string(),
        None => default.to_string(),
    }
}

/// config 层的标量转字符串（bool → Python 形态）。
fn py_str_config(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn load_config(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
    let source = path.as_ref().canonicalize().map_err(|_| {
        ConfigError::new(format!(
            "configuration file not found: {}",
            path.as_ref().display()
        ))
    })?;
    let text = std::fs::read_to_string(&source)
        .map_err(|err| ConfigError::new(format!("configuration file not readable: {err}")))?;
    let raw: Value = serde_yml::from_str(&text)
        .map_err(|err| ConfigError::new(format!("configuration parse error: {err}")))?;
    if !raw.is_object() {
        return Err(ConfigError::new("configuration root must be a mapping"));
    }
    if raw.get("version") != Some(&Value::from(6)) {
        return Err(ConfigError::new("'version' must be 6"));
    }

    let project = mapping(&raw, "project", None)?;
    let bilibili = mapping(&raw, "bilibili", None)?;
    let collection = mapping(&raw, "collection", None)?;
    let perception = mapping(&raw, "perception", Some(empty_mapping()))?;
    let peer = mapping(perception, "peer_discovery", Some(empty_mapping()))?;
    let ai = mapping(&raw, "ai", None)?;
    let report = mapping(&raw, "report", None)?;

    let removed_keys: Vec<String> = [
        (collection, &["max_video_tag_requests"][..]),
        (ai, &["max_tokens", "max_evidence_per_viewer"][..]),
    ]
    .into_iter()
    .flat_map(|(section, names)| {
        names
            .iter()
            .filter(|name| section.get(**name).is_some())
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    })
    .collect();
    if !removed_keys.is_empty() {
        return Err(ConfigError::new(format!(
            "removed configuration key: {}",
            removed_keys.join(", ")
        )));
    }

    let additional_ids: Vec<String> = match bilibili.get("additional_viewer_ids") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut seen: Vec<String> = Vec::new();
            for item in items {
                let text = py_str_config(item).trim().to_string();
                if !text.is_empty() && !seen.contains(&text) {
                    seen.push(text);
                }
            }
            seen
        }
        Some(_) => {
            return Err(ConfigError::new(
                "'bilibili.additional_viewer_ids' must be a list",
            ));
        }
    };

    let reasoning = mapping(ai, "reasoning", None)?;
    let runtime = mapping(ai, "agent", None)?;
    let api = string(ai, "api", "");
    if !ALLOWED_APIS.contains(&api.as_str()) {
        return Err(ConfigError::new(
            "'ai.api' must be 'chat_completions' or 'responses'",
        ));
    }
    let effort = string(reasoning, "effort", "high");
    if !ALLOWED_EFFORTS.contains(&effort.as_str()) {
        return Err(ConfigError::new("'ai.reasoning.effort' is invalid"));
    }

    let rules: Vec<String> = match ai.get("rules") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| py_str_config(item).trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        Some(_) => return Err(ConfigError::new("'ai.rules' must be a list")),
    };

    let output_value = string(project, "output_dir", "");
    if output_value.is_empty() {
        return Err(ConfigError::new("'project.output_dir' cannot be empty"));
    }
    let expanded = shellexpand_tilde(&output_value);
    let output_dir = if Path::new(&expanded).is_absolute() {
        PathBuf::from(expanded)
    } else {
        source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(expanded)
    };
    // resolve：不存在的路径不能 canonicalize，用规范化展开即可（Python 行为是 resolve，
    // 但 resolve 不校验存在性——rust 等价物 manual normalize）。
    let project_name = {
        let name = string(project, "name", "audience-perception");
        if name.is_empty() {
            "audience-perception".to_string()
        } else {
            name
        }
    };
    let report_title = {
        let title = string(report, "title", "直播观众态势感知");
        if title.is_empty() {
            "直播观众态势感知".to_string()
        } else {
            title
        }
    };

    Ok(Config {
        source,
        project_name,
        output_dir,
        bilibili: BilibiliConfig {
            room_id: string(bilibili, "room_id", ""),
            streamer_uid: string(bilibili, "streamer_uid", ""),
            cookie: string(bilibili, "cookie", ""),
            additional_viewer_ids: additional_ids,
        },
        collection: CollectionConfig {
            max_guards: integer(collection, "max_guards", 1, None)?,
            per_viewer_request_budget: integer(collection, "per_viewer_request_budget", 1, None)?,
            followings_limit: integer(collection, "followings_limit", 0, None)?,
            recent_videos: integer(collection, "recent_videos", 0, None)?,
            recent_dynamics: integer(collection, "recent_dynamics", 0, None)?,
            favorite_folders: integer(collection, "favorite_folders", 0, None)?,
            favorite_items_per_folder: integer(collection, "favorite_items_per_folder", 0, None)?,
            bangumi_limit: integer(collection, "bangumi_limit", 0, None)?,
            games_limit: integer(collection, "games_limit", 0, None)?,
            max_video_metadata_items: integer(collection, "max_video_metadata_items", 0, Some(80))?,
            request_delay_seconds: number(collection, "request_delay_seconds", 0.0, None)?,
            timeout_seconds: number(collection, "timeout_seconds", 1.0, None)?,
            room_comment_request_budget: integer(
                collection,
                "room_comment_request_budget",
                0,
                Some(3),
            )?,
            live_replay_danmaku_limit: integer(
                collection,
                "live_replay_danmaku_limit",
                0,
                Some(20),
            )?,
            lead_fetch_budget_per_run: integer(
                collection,
                "lead_fetch_budget_per_run",
                0,
                Some(0),
            )?,
        },
        perception: PerceptionConfig {
            max_evidence_per_viewer: integer(perception, "max_evidence_per_viewer", 0, Some(1000))?,
            preserve_raw_snapshots: boolean(perception, "preserve_raw_snapshots", true)?,
            platform_hot_search_limit: integer(
                perception,
                "platform_hot_search_limit",
                0,
                Some(50),
            )?,
            minimum_community_size: integer(perception, "minimum_community_size", 1, Some(1))?,
            peer: PeerDiscoveryConfig {
                candidate_limit: integer(peer, "candidate_limit", 1, Some(20))?,
                recent_videos: integer(peer, "recent_videos", 1, Some(8))?,
                recent_dynamics: integer(peer, "recent_dynamics", 1, Some(8))?,
                max_formal_peers: integer(peer, "max_formal_peers", 1, Some(8))?,
            },
        },
        ai: AiConfig {
            base_url: string(ai, "base_url", "").trim_end_matches('/').to_string(),
            api_key: string(ai, "api_key", ""),
            model: string(ai, "model", ""),
            timeout_seconds: number(ai, "timeout_seconds", 1.0, Some(900.0))?,
            max_output_tokens: integer(ai, "max_output_tokens", 1, Some(131_072))?,
            reasoning: ReasoningConfig {
                enabled: boolean(reasoning, "enabled", true)?,
                effort,
                replay_content: boolean(reasoning, "replay_content", true)?,
            },
            agent: AgentRuntimeConfig {
                max_turns: integer(runtime, "max_turns", 2, Some(64))?,
                resume: boolean(runtime, "resume", true)?,
                local_trace: boolean(runtime, "local_trace", true)?,
                run_retries: integer(runtime, "run_retries", 0, Some(2))?,
                retry_backoff_seconds: number(runtime, "retry_backoff_seconds", 0.0, Some(3.0))?,
            },
            search_results_per_query: integer(ai, "search_results_per_query", 1, Some(20))?,
            rules,
            api,
        },
        report_title,
    })
}

fn shellexpand_tilde(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return format!("{}/{rest}", PathBuf::from(home).display());
    }
    value.to_string()
}

/// Python `cookie_names`：Cookie 字符串中的键集合。
pub fn cookie_names(cookie: &str) -> std::collections::BTreeSet<String> {
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

pub fn collection_issues(config: &Config) -> Vec<String> {
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

pub fn ai_issues(config: &Config) -> Vec<String> {
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
    if config.ai.api == "responses" && config.ai.reasoning.replay_content {
        issues.push("ai.reasoning.replay_content only applies to chat_completions".to_string());
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
        "max_turns".to_string(),
        Value::from(config.ai.agent.max_turns),
    );
    agent.insert(
        "run_retries".to_string(),
        Value::from(config.ai.agent.run_retries),
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
    use super::*;

    const EXAMPLE_YAML: &str = r#"
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
    max_turns: 64
    run_retries: 2
  max_output_tokens: 131072
report:
  title: 直播观众态势感知
"#;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), content).unwrap();
        file
    }

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
    fn rejects_removed_keys() {
        let bad = EXAMPLE_YAML.replace("max_output_tokens", "max_tokens");
        let file = write_temp(&bad);
        let err = load_config(file.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("removed configuration key: max_tokens")
        );
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
