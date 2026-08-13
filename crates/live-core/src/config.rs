//! 运行配置：serde_yml 装载（移植 Python `config.py`）。
//!
//! 体积备书已兑现：原 1045 行超 800 行红线，拆出 check.rs（校验/归一化/快照，fixture
//! 对账、非秘密处理）与 testkit.rs（测试共享 YAML/临时文件）；根只留类型 + 装载。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

mod check;

// 收窄：cookie_names/issues×2 仓库外零消费（grep 实证）→ 降 pub(crate)，不爬公面；
// normalized_json 的具名消费者 = fixture parity 钉（example.normalized.json 子集相等，
// M1 裁决：config 字段=外部承诺面+fixture 钉住）→ 留公面。
pub use check::{normalized_json, validate_for_ai, validate_for_collection};

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
    /// 整体图谱默认展开节点类白名单——Episode/Mention 细节层默认折叠，
    /// 前端整体图首载只看 Viewer/Entity/状态-行动层。查询参数 `?kinds=all` 可逃回全量。
    /// 配置值必须属于 [`GRAPH_KIND_ALLOWLIST`]；特殊单值 "all" 展开为全部类。
    pub graph_default_expanded_kinds: Vec<String>,
    /// graph 读面各臂（nodes/edges/mentions/episodes）单行帽
    /// （Python graph.AUDIENCE_GRAPH_LIMIT 归配）；>=1。与 GRAPH_QUERY_LIMIT=500
    /// （Python 查询口径钳制，parity 钉）是两条独立闸线，本键只治理「面板导出行帽」。
    pub graph_row_limit: i64,
    pub peer: PeerDiscoveryConfig,
}

/// 图节点类受控全集（node_type 全谱；InterestState/Situation/Action 历史上可作节点出现）。
pub const GRAPH_KIND_ALLOWLIST: [&str; 7] = [
    "Viewer",
    "Entity",
    "Episode",
    "Mention",
    "InterestState",
    "Situation",
    "Action",
];
/// 默认展开白名单（细节层 Episode/Mention 折叠：实测 s0 元素数 −75%、字节 −79%）。
pub const GRAPH_DEFAULT_EXPANDED_KINDS: [&str; 5] =
    ["Viewer", "Entity", "InterestState", "Situation", "Action"];
/// 面板导出行帽默认值（Python graph.AUDIENCE_GRAPH_LIMIT 的同语义迁移锚）。
pub const DEFAULT_GRAPH_ROW_LIMIT: i64 = 5_000;

pub const ALLOWED_EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone)]
pub struct ReasoningConfig {
    pub enabled: bool,
    pub effort: String,
    pub replay_content: bool,
    /// reasoning 回放窗口。None=不限窗（现行逐字回放）；Some(k)=仅末 k 条带 tool_calls
    /// 的 assistant 保留原文，更老轮保留字段但置空串（dsv4 「字段必现」安全形状）。
    /// 仅当 replay_content=true 时有效（false 时历史已被剥成 None，无物可窗）。
    pub replay_window: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct AgentRuntimeConfig {
    pub resume: bool,
    pub local_trace: bool,
    pub run_retries: i64,
    pub retry_backoff_seconds: f64,
    /// 单观众 token 预算熔断：默认 200_000（u32 域，非负），该 viewer agent
    /// 每轮 LLM 请求后核对累计 total_tokens，超限即触顶终止并记 viewer_failure。
    pub viewer_token_budget: u32,
    /// 闸门一：并行 viewer agent 上限（Semaphore 许可数）。默认 4
    /// （Python asyncio.Semaphore(4)，ADR-0004 同构；INVESTIGATE_CONCURRENCY 为其锚）。
    pub max_parallel_viewers: i64,
    /// 闸门二：全 run 级 LLM 出队请求上限（requests/min，漏桶）。
    /// 0 = 关闭（默认，S0 实测零 429 故默认既不限速也不改变既有行为）；>0 时
    /// 每个 LLM 请求前 acquire 一个许可，许可即请求 1:1。
    pub max_llm_rpm: i64,
    /// 中间轮折叠阈值（估算 tokens，字节秤/4）。0=关闭（默认）。
    pub fold_trigger_tokens: u32,
    /// 折叠后保留末尾完整轮数。默认 2。
    pub fold_keep_tail_turns: usize,
    /// 折叠摘要单轮条目字符预算。默认 480。
    pub fold_entry_chars: usize,
}

impl AgentRuntimeConfig {
    /// 折叠配置仅在 trigger>0 时启用（默认关）。
    pub fn fold_config(&self) -> Option<crate::agent::runtime::FoldConfig> {
        if self.fold_trigger_tokens == 0 {
            None
        } else {
            Some(crate::agent::runtime::FoldConfig {
                trigger_tokens: self.fold_trigger_tokens,
                keep_tail_turns: self.fold_keep_tail_turns,
                entry_chars: self.fold_entry_chars,
            })
        }
    }
}

pub const ALLOWED_APIS: [&str; 1] = ["chat_completions"];

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
    /// 白名单第 5 键：每轮 run 花费预算（¥）。None=不设闸（现状一字不动）；
    /// Some 时 per-viewer 全量预估严格超此额即阻断并提供省钱模式。字符串语义（金额文案，
    /// 与 YAML 数字型形义区分），同现有 4 键白名单规格、非法即 422 同族。
    pub run_budget_cny: Option<f64>,
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

/// 可选整型：缺键→None；存在但无法解析→错误；用于 reasoning.replay_window。
fn optional_integer(mapping: &Value, key: &str) -> Result<Option<i64>, ConfigError> {
    match mapping.get(key) {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| ConfigError::new(format!("'{key}' must be an integer")))
            .map(Some),
        Some(_) => Err(ConfigError::new(format!("'{key}' must be an integer"))),
    }
}

fn string(mapping: &Value, key: &str, default: &str) -> String {
    match mapping.get(key) {
        Some(value) => py_str_config(value).trim().to_string(),
        None => default.to_string(),
    }
}

/// `run_budget_cny` 只收**字符串**金额（白名单第 5 键字符串语义，与 YAML
/// 数字型形义区分——金额文案应写作 "3.50" 而非 3.50）。缺键/空串→None（不设闸）；
/// 解析失败、NaN/inf、负值、超上限一律拒装（同现有白名单键 422 规格）。
fn optional_money_string(mapping: &Value, key: &str) -> Result<Option<f64>, ConfigError> {
    const MAX_BUDGET_CNY: f64 = 1_000_000.0;
    match mapping.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let value = trimmed.parse::<f64>().map_err(|_| {
                ConfigError::new(format!("'{key}' must be a money string: \"{s}\""))
            })?;
            if !value.is_finite() || !(0.0..=MAX_BUDGET_CNY).contains(&value) {
                return Err(ConfigError::new(format!(
                    "'{key}' must be a finite money amount in [0, {MAX_BUDGET_CNY}]"
                )));
            }
            Ok(Some(value))
        }
        Some(_) => Err(ConfigError::new(format!(
            "'{key}' must be a string (money amount in CNY)"
        ))),
    }
}

/// i64 配置值钳入 u32/usize 的公共件（原五处就地 clamp+as 同款样板）。
fn clamped_u32(value: i64, lo: u32, hi: u32) -> u32 {
    value.clamp(lo as i64, hi as i64) as u32
}

fn clamped_usize(value: i64, lo: usize, hi: usize) -> usize {
    value.clamp(lo as i64, hi as i64) as usize
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

    let runtime = mapping(ai, "agent", None)?;
    let removed_keys: Vec<String> = [
        (collection, &["max_video_tag_requests"][..]),
        (ai, &["max_tokens", "max_evidence_per_viewer"][..]),
        // 2026-08-13：单键催交线退役——三级阶梯（13 劝/37 限期/40 必收）收敛入
        // runtime.rs 常量；轮数语义由官规统一裁决，不再留给各部署各自为政。
        (runtime, &["wrap_up_reminder_turn"][..]),
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
        return Err(ConfigError::new("'ai.api' must be 'chat_completions'"));
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
            graph_default_expanded_kinds: {
                let kinds: Vec<String> = match perception.get("graph_default_expanded_kinds") {
                    None => GRAPH_DEFAULT_EXPANDED_KINDS
                        .iter()
                        .map(|kind| kind.to_string())
                        .collect(),
                    Some(Value::Array(items)) => {
                        let mut seen: Vec<String> = Vec::new();
                        for item in items {
                            let kind = py_str_config(item).trim().to_string();
                            if kind.is_empty() || seen.contains(&kind) {
                                continue;
                            }
                            if !GRAPH_KIND_ALLOWLIST.contains(&kind.as_str()) {
                                return Err(ConfigError::new(format!(
                                    "'perception.graph_default_expanded_kinds' 未知节点类 \
                                     \"{kind}\"（允许：{}；或单值 all = 全谱）",
                                    GRAPH_KIND_ALLOWLIST.join("/"),
                                )));
                            }
                            seen.push(kind);
                        }
                        if seen.is_empty() {
                            return Err(ConfigError::new(
                                "'perception.graph_default_expanded_kinds' 不可为空列表",
                            ));
                        }
                        seen
                    }
                    // 单值 "all" = 全谱逃生门（与白名单全集等价，刻意不止收进查询参数）。
                    Some(Value::String(s)) if s.trim() == "all" => GRAPH_KIND_ALLOWLIST
                        .iter()
                        .map(|kind| kind.to_string())
                        .collect(),
                    Some(_) => {
                        return Err(ConfigError::new(
                            "'perception.graph_default_expanded_kinds' must be a list 或 \"all\"",
                        ));
                    }
                };
                kinds
            },
            graph_row_limit: integer(
                perception,
                "graph_row_limit",
                1,
                Some(DEFAULT_GRAPH_ROW_LIMIT),
            )?,
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
                replay_window: optional_integer(reasoning, "replay_window")?
                    .map(|v| clamped_u32(v, 1, u32::MAX)),
            },
            agent: AgentRuntimeConfig {
                resume: boolean(runtime, "resume", true)?,
                local_trace: boolean(runtime, "local_trace", true)?,
                run_retries: integer(runtime, "run_retries", 0, Some(2))?,
                retry_backoff_seconds: number(runtime, "retry_backoff_seconds", 0.0, Some(3.0))?,
                viewer_token_budget: clamped_u32(
                    integer(runtime, "viewer_token_budget", 0, Some(200_000))?,
                    0,
                    u32::MAX,
                ),
                fold_trigger_tokens: clamped_u32(
                    integer(runtime, "fold_trigger_tokens", 0, Some(0))?,
                    0,
                    u32::MAX,
                ),
                fold_keep_tail_turns: clamped_usize(
                    integer(runtime, "fold_keep_tail_turns", 0, Some(2))?,
                    1,
                    64,
                ),
                fold_entry_chars: clamped_usize(
                    integer(runtime, "fold_entry_chars", 0, Some(480))?,
                    32,
                    8192,
                ),
                max_parallel_viewers: integer(runtime, "max_parallel_viewers", 1, Some(4))?,
                max_llm_rpm: integer(runtime, "max_llm_rpm", 0, Some(0))?,
            },
            search_results_per_query: integer(ai, "search_results_per_query", 1, Some(20))?,
            rules,
            run_budget_cny: optional_money_string(ai, "run_budget_cny")?,
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

#[cfg(test)]
mod testkit;

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

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

    /// M4.x budget 键拒负（与其他 collection 键同族校验，补钉）。
    #[test]
    fn lead_fetch_budget_per_run_rejects_negative() {
        let bad = EXAMPLE_YAML.replace(
            "max_video_metadata_items: 120",
            "max_video_metadata_items: 120\n  lead_fetch_budget_per_run: -1",
        );
        let file = write_temp(&bad);
        let err = load_config(file.path()).unwrap_err();
        assert!(
            err.to_string().contains("lead_fetch_budget_per_run"),
            "{err}"
        );
        // 缺省 = 0＝休眠（M4.x 默认人工审批文化）
        let ok = load_config(write_temp(EXAMPLE_YAML).path()).unwrap();
        assert_eq!(ok.collection.lead_fetch_budget_per_run, 0);
    }

    /// `ai.agent.viewer_token_budget` 默认 200_000；可用键覆写；拒绝负值。
    #[test]
    fn viewer_token_budget_default_override_and_reject_negative() {
        let ok = load_config(write_temp(EXAMPLE_YAML).path()).unwrap();
        assert_eq!(ok.ai.agent.viewer_token_budget, 200_000);

        let overridden = EXAMPLE_YAML.replace(
            "    run_retries: 2",
            "    run_retries: 2\n    viewer_token_budget: 8000",
        );
        let ok2 = load_config(write_temp(&overridden).path()).unwrap();
        assert_eq!(ok2.ai.agent.viewer_token_budget, 8000);

        let bad = EXAMPLE_YAML.replace(
            "    run_retries: 2",
            "    run_retries: 2\n    viewer_token_budget: -5",
        );
        let err = load_config(write_temp(&bad).path()).unwrap_err();
        assert!(err.to_string().contains("viewer_token_budget"), "{err}");
    }

    /// 2026-08-13：单键催交线退役为 removed 键——三级阶梯（13 劝/37 限期/40 必收）
    /// 收敛入 runtime 常量；旧部署残留必须给出具名错误（杜绝第二次无声死键）。
    #[test]
    fn wrap_up_reminder_turn_is_a_removed_key() {
        let bad = EXAMPLE_YAML.replace(
            "    run_retries: 2",
            "    run_retries: 2\n    wrap_up_reminder_turn: 24",
        );
        let err = load_config(write_temp(&bad).path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("removed configuration key: wrap_up_reminder_turn"),
            "{err}"
        );
    }

    /// 删码专项：ai.api 白名单只剩 chat_completions（responses 死分支已拔）——
    /// 非法值加载期即拒（原为加载软过 + runtime 启动硬拒的双层形态）。
    #[test]
    fn ai_api_rejects_non_chat_completions_at_load() {
        let ok = load_config(write_temp(EXAMPLE_YAML).path()).unwrap();
        assert_eq!(ok.ai.api, "chat_completions");
        let bad = EXAMPLE_YAML.replace("api: chat_completions", "api: responses");
        let err = load_config(write_temp(&bad).path()).unwrap_err();
        assert!(err.to_string().contains("ai.api"), "{err}");
    }

    /// graph_default_expanded_kinds 默认五类白名单；"all" 展开七类全谱；
    /// 未知类/空列表/非列表一律拒装。graph_row_limit 默认 5000、可覆盖、拒 0。
    #[test]
    fn graph_fold_and_row_limit_default_override_and_reject() {
        let ok = load_config(write_temp(EXAMPLE_YAML).path()).unwrap();
        assert_eq!(
            ok.perception.graph_default_expanded_kinds,
            vec!["Viewer", "Entity", "InterestState", "Situation", "Action"]
        );
        assert_eq!(ok.perception.graph_row_limit, 5_000);

        let overridden = EXAMPLE_YAML.replace(
            "  peer_discovery:",
            "  graph_default_expanded_kinds: [Viewer, Entity, Episode]\n  graph_row_limit: 2000\n  peer_discovery:",
        );
        let ok2 = load_config(write_temp(&overridden).path()).unwrap();
        assert_eq!(
            ok2.perception.graph_default_expanded_kinds,
            vec!["Viewer", "Entity", "Episode"]
        );
        assert_eq!(ok2.perception.graph_row_limit, 2000);

        let all = EXAMPLE_YAML.replace(
            "  peer_discovery:",
            "  graph_default_expanded_kinds: all\n  peer_discovery:",
        );
        let ok3 = load_config(write_temp(&all).path()).unwrap();
        assert_eq!(ok3.perception.graph_default_expanded_kinds.len(), 7);

        for snippet in [
            "  graph_default_expanded_kinds: [Viewer, Unicorn]",
            "  graph_default_expanded_kinds: []",
            "  graph_default_expanded_kinds: 42",
            "  graph_row_limit: 0",
        ] {
            let bad = EXAMPLE_YAML.replace(
                "  peer_discovery:",
                &format!("{snippet}\n  peer_discovery:"),
            );
            let err = load_config(write_temp(&bad).path()).unwrap_err();
            assert!(
                err.to_string().contains("graph_default_expanded_kinds")
                    || err.to_string().contains("graph_row_limit"),
                "{snippet} 必须拒装：{err}"
            );
        }
    }

    /// `ai.agent.max_parallel_viewers` 默认 4（最低 1）；
    /// `max_llm_rpm` 默认 0（关闭），负值拒绝。
    #[test]
    fn elevator_gates_default_override_and_reject() {
        let ok = load_config(write_temp(EXAMPLE_YAML).path()).unwrap();
        assert_eq!(ok.ai.agent.max_parallel_viewers, 4);
        assert_eq!(ok.ai.agent.max_llm_rpm, 0);

        let overridden = EXAMPLE_YAML.replace(
            "    run_retries: 2",
            "    run_retries: 2\n    max_parallel_viewers: 2\n    max_llm_rpm: 300",
        );
        let ok2 = load_config(write_temp(&overridden).path()).unwrap();
        assert_eq!(ok2.ai.agent.max_parallel_viewers, 2);
        assert_eq!(ok2.ai.agent.max_llm_rpm, 300);

        for (key, value) in [("max_parallel_viewers", "0"), ("max_llm_rpm", "-10")] {
            let bad = EXAMPLE_YAML.replace(
                "    run_retries: 2",
                &format!("    run_retries: 2\n    {key}: {value}"),
            );
            let err = load_config(write_temp(&bad).path()).unwrap_err();
            assert!(err.to_string().contains(key), "{key}: {err}");
        }
    }

    /// `ai.run_budget_cny` 白名单第 5 键——缺省 None（不设闸，现状一字不动）；
    /// 字符串金额回显 Some；空串→None；非字符串（数字型 YAML）/非法/负值/非有限一律拒装。
    #[test]
    fn run_budget_cny_default_string_and_domain_rejects() {
        let ok = load_config(write_temp(EXAMPLE_YAML).path()).unwrap();
        assert_eq!(ok.ai.run_budget_cny, None, "缺省 None=不设闸（现状文化）");

        let with = EXAMPLE_YAML.replace(
            "    run_retries: 2",
            "    run_retries: 2\n  run_budget_cny: \"3.50\"",
        );
        let ok2 = load_config(write_temp(&with).path()).unwrap();
        assert_eq!(ok2.ai.run_budget_cny, Some(3.5), "字符串金额回显 Some");

        let empty = EXAMPLE_YAML.replace(
            "    run_retries: 2",
            "    run_retries: 2\n  run_budget_cny: \"\"",
        );
        let ok3 = load_config(write_temp(&empty).path()).unwrap();
        assert_eq!(ok3.ai.run_budget_cny, None, "空串=不设闸");

        for injected in [
            "  run_budget_cny: 3.5",
            "  run_budget_cny: \"-1\"",
            "  run_budget_cny: \"abc\"",
            "  run_budget_cny: \"NaN\"",
            "  run_budget_cny: true",
        ] {
            let bad = EXAMPLE_YAML.replace(
                "    run_retries: 2",
                &format!("    run_retries: 2\n{injected}"),
            );
            let err = load_config(write_temp(&bad).path()).unwrap_err();
            assert!(
                err.to_string().contains("run_budget_cny"),
                "{injected} 必须拒装：{err}"
            );
        }
    }
}
