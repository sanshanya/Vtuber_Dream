//! M3-B 调查工具面（只读）+ ResearchService（缓存 / 按运行隔离的注册表 / 搜索快照）。
//!
//! 原语化定界（design §7.9 开工预审结论，docs/2026-08-04-m3-kickoff.md）：
//! - `search_bilibili_videos` 为**聚合型**：平台搜索原语外加三块程序职责——query 截断、
//!   order 白名单回落、limit 双层钳制 + 结果注册（按运行隔离，修复6）+ 快照落盘
//!   `searches/{search_id}.json`（修复7）+ research_cache.json 持久缓存；
//! - 其余 4 个为**原语**：图读 / 文件读 / 详情缓存读，无任何编排；
//! - 终局校验台在 validators（M3-C）；Peer 链三工具延迟到 G2（design §3 范围外）。
//!
//! Python 对应物：tools.py 的 ResearchService + five @function_tool。Rust 工具参数从
//! JSON args 手动取值（Python 签名级默认值逐一对齐）。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::bilibili::{BilibiliClient, BilibiliError};
use crate::episodes::{Episode, char_prefix, hash_parts, py_str};
use crate::graph::query;
use crate::graph::store::Store;
use crate::models::{AudienceSituationSubmission, ViewerPerceptionSubmission};
use crate::storage::{read_json, write_json};

use super::prompts::{audience_instructions, viewer_instructions};
use super::runtime::{AgentSpec, AgentTool, RunCtx, SubmissionSlot, make_terminal_tool};
use super::validators::{validate_audience_submission, validate_viewer_submission};

// ---------------------------------------------------------------------------
// 命名阈值（魔数条款：每项有命名/默认值/用途/测试）
// ---------------------------------------------------------------------------

/// 搜索 query 截断（Python `query.strip()[:500]`）。
pub const SEARCH_QUERY_MAX_CHARS: usize = 500;
/// bvid 截断（Python `bvid.strip()[:100]`）。
pub const BVID_MAX_CHARS: usize = 100;
/// search_entity_candidates 的 limit 钳制上界（Python `max(1, min(limit, 100))`）。
pub const SEARCH_ENTITY_LIMIT_CAP: i64 = 100;
/// 搜索 limit 的双层钳制上界（Python `min(limit, per_query_cap, 50)` 的常数 50）。
pub const SEARCH_LIMIT_HARD_CAP: i64 = 50;
/// search_bilibili_videos 的默认 limit（Python 函数签名默认值）。
pub const SEARCH_DEFAULT_LIMIT: i64 = 10;
/// result_id 形态：程序生成的 sha1 hex 前 16 位（安全批 R1：注册表/快照只接受此形态，
/// 防 cache 文件被篡改后注入目录穿越 id 或伪造可引用 id）。
pub const SEARCH_RESULT_ID_CHARS: usize = 16;
/// get_viewer_analysis 附带 episode 的条数钳制（Python `min(episode_limit, 10)`）。
pub const VIEWER_EPISODE_LIMIT_CAP: i64 = 10;
/// query_graph 默认 limit（Python 签名默认 500 = GRAPH_QUERY_LIMIT）。
const QUERY_GRAPH_DEFAULT_LIMIT: i64 = 500;

const SEARCH_ORDER_WHITELIST: [&str; 4] = ["totalrank", "pubdate", "click", "stow"];

const TAKE_LIMIT_SEARCH_TITLE: usize = 4_000;
const TAKE_LIMIT_SEARCH_DESC: usize = 10_000;
const TAKE_LIMIT_CLIP_SMALL: usize = 1_000;
const TAKE_LIMIT_ID: usize = 100;

// ---------------------------------------------------------------------------
// 文本基元（Python ai_data.clip / normalize_search_result）
// ---------------------------------------------------------------------------

fn ws_collapse_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").expect("ws regex"))
}

fn tag_strip_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("tag strip regex"))
}

/// Python `str(value or "")`：falsy（None/false/0/""/[]/{}）→ ""，其余 str()。
fn py_or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => String::new(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => py_str(other),
    }
}

/// Python `clip(value, limit)`：`\s+` 折叠 → strip → 字符截断。
fn clip(value: Option<&Value>, limit: usize) -> String {
    let raw = py_or_empty(value);
    let collapsed = ws_collapse_re().replace_all(&raw, " ");
    char_prefix(collapsed.trim(), limit)
}

/// Python `normalize_search_result`（ai_data.py:184）逐行移植。
pub fn normalize_search_result(query: &str, index: usize, item: &Value) -> Value {
    let bvid = clip(item.get("bvid"), TAKE_LIMIT_ID);
    let aid = clip(item.get("aid"), TAKE_LIMIT_ID);
    let anchor = if bvid.is_empty() { &aid } else { &bvid };
    let result_id = hash_parts(
        &[query.to_string(), anchor.clone(), (index + 1).to_string()],
        16,
    );
    let title = tag_strip_re()
        .replace_all(&py_or_empty(item.get("title")), "")
        .to_string();
    json!({
        "result_id": result_id,
        "query": query,
        "title": clip(Some(&Value::String(title)), TAKE_LIMIT_SEARCH_TITLE),
        "bvid": bvid,
        "aid": aid,
        "url": if bvid.is_empty() {
            String::new()
        } else {
            format!("https://www.bilibili.com/video/{bvid}")
        },
        "author": clip(item.get("author"), TAKE_LIMIT_CLIP_SMALL),
        "author_id": clip(item.get("mid"), TAKE_LIMIT_ID),
        "description": clip(item.get("description"), TAKE_LIMIT_SEARCH_DESC),
        "tags": clip(item.get("tag"), TAKE_LIMIT_SEARCH_TITLE),
        "type_name": clip(item.get("typename"), TAKE_LIMIT_CLIP_SMALL),
        "play": item.get("play").cloned().unwrap_or(Value::Null),
        "favorites": item.get("favorites").cloned().unwrap_or(Value::Null),
        "pubdate": item.get("pubdate").cloned().unwrap_or(Value::Null),
    })
}

// ---------------------------------------------------------------------------
// ResearchService（缓存 + 注册表 + 快照；修复6 运行隔离 / 修复7 快照落盘）
// ---------------------------------------------------------------------------

/// AI 调查期的检索服务。注册表（`search_results`）为**按运行实例隔离**——
/// 不随 ResearchService 克隆共享；但会从 research_cache.json 回填（Python 一致：
/// 跨运行引用凭快照文件可回访——修复7 的语义补充）。
/// result_id 形态闸：恰好 16 位小写 hex（hash_parts 产出形态）。
fn is_search_result_id(id: &str) -> bool {
    id.len() == SEARCH_RESULT_ID_CHARS
        && id
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
}

pub struct ResearchService {
    cache_path: PathBuf,
    searches_dir: PathBuf,
    cache: Value,
    /// 本次运行可引用的 result_id → 结果行（终局校验的引用闭包来源）。
    pub search_results: BTreeMap<String, Value>,
    pub client: BilibiliClient,
    per_query_cap: i64,
}

impl ResearchService {
    pub fn new(output_dir: &Path, client: BilibiliClient, per_query_cap: i64) -> Self {
        let cache_path = output_dir.join("ai").join("research_cache.json");
        let searches_dir = output_dir.join("ai").join("searches");
        let mut cache = read_json(&cache_path).ok().flatten().unwrap_or(json!({}));
        if !cache.is_object() {
            cache = json!({});
        }
        for bucket in ["searches", "videos"] {
            if cache.get(bucket).and_then(Value::as_object).is_none() {
                cache[bucket] = json!({});
            }
        }
        let mut search_results = BTreeMap::new();
        if let Some(searches) = cache["searches"].as_object() {
            for rows in searches.values() {
                if let Some(list) = rows.as_array() {
                    for item in list {
                        if let Some(id) = item.get("result_id").and_then(Value::as_str)
                            && is_search_result_id(id)
                        {
                            search_results.insert(id.to_string(), item.clone());
                        }
                    }
                }
            }
        }
        Self {
            cache_path,
            searches_dir,
            cache,
            search_results,
            client,
            per_query_cap,
        }
    }

    fn save(&self) {
        let _ = write_json(&self.cache_path, &self.cache);
    }

    /// 修复7：把可引用的搜索结果归档到 `searches/{result_id}.json`（幂等：已存在不重写）。
    fn snapshot(&self, row: &Value) {
        if let Some(id) = row.get("result_id").and_then(Value::as_str)
            && is_search_result_id(id)
        {
            let path = self.searches_dir.join(format!("{id}.json"));
            if !path.is_file() {
                let _ = write_json(&path, row);
            }
        }
    }

    /// Python `ResearchService.search`：截断 → 白名单 → 钳制 → 缓存命中即返回 →
    /// 否则调用搜索端点、归一、注册、快照、落缓存。
    pub fn search(
        &mut self,
        query: &str,
        order: &str,
        limit: i64,
    ) -> Result<Vec<Value>, BilibiliError> {
        let query = char_prefix(query.trim(), SEARCH_QUERY_MAX_CHARS);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let safe_order = if SEARCH_ORDER_WHITELIST.contains(&order) {
            order
        } else {
            "totalrank"
        };
        let safe_limit = limit.clamp(1, self.per_query_cap.clamp(1, SEARCH_LIMIT_HARD_CAP));
        let key = format!("{query}|{safe_order}|{safe_limit}");
        if let Some(cached) = self.cache["searches"].get(&key).and_then(Value::as_array) {
            for row in cached {
                if let Some(id) = row.get("result_id").and_then(Value::as_str) {
                    self.search_results.insert(id.to_string(), row.clone());
                }
                self.snapshot(row);
            }
            return Ok(cached.clone());
        }
        let raw = self.client.search_videos(&query, safe_limit, safe_order)?;
        let rows: Vec<Value> = raw
            .iter()
            .enumerate()
            .map(|(index, item)| normalize_search_result(&query, index, item))
            .collect();
        self.cache["searches"][key] = Value::Array(rows.clone());
        for row in &rows {
            if let Some(id) = row.get("result_id").and_then(Value::as_str) {
                self.search_results.insert(id.to_string(), row.clone());
            }
            self.snapshot(row);
        }
        self.save();
        Ok(rows)
    }

    /// Python `ResearchService.video`：详情 + TAG 聚合缓存。空 bvid → 空对象（零请求）。
    pub fn video(&mut self, bvid: &str) -> Result<Value, BilibiliError> {
        let bvid = char_prefix(bvid.trim(), BVID_MAX_CHARS);
        if bvid.is_empty() {
            return Ok(json!({}));
        }
        if let Some(cached) = self.cache["videos"].get(&bvid).and_then(Value::as_object) {
            return Ok(Value::Object(cached.clone()));
        }
        let detail = self.client.video_detail(&bvid)?;
        let tags = self.client.video_tags(&bvid)?;
        let get = |key: &str| detail.get(key).cloned().unwrap_or(Value::Null);
        let int_or_zero = |v: &Value| v.as_i64().unwrap_or(0);
        let result = json!({
            "bvid": bvid,
            "title": py_str(&get("title")),
            "description": py_str(&get("desc")),
            "owner": if get("owner").is_object() { get("owner") } else { json!({}) },
            "stat": if get("stat").is_object() { get("stat") } else { json!({}) },
            "pubdate": get("pubdate"),
            "platform_category": {
                "id": int_or_zero(&get("tid")),
                "name": py_str(&get("tname")),
                "parent_id": int_or_zero(&get("parent_tid")),
                "v2_name": py_str(&get("tname_v2")),
            },
            "tags": tags,
            "url": format!("https://www.bilibili.com/video/{bvid}"),
        });
        self.cache["videos"][bvid] = result.clone();
        self.save();
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Agent 上下文 + 能力 trait（M3-C 终局校验台仍挂 SubmissionSlot）
// ---------------------------------------------------------------------------

/// 个人感知 Agent 上下文：viewer 事实 + Episode 注册表 + 图 + 调查服务。
pub struct ViewerAgentCtx {
    pub viewer_data: Value,
    pub episodes: BTreeMap<String, Episode>,
    pub research: ResearchService,
    pub store: Store,
    pub slot: SubmissionSlot,
}

impl RunCtx for ViewerAgentCtx {
    fn slot(&mut self) -> &mut SubmissionSlot {
        &mut self.slot
    }
}

/// 整体态势 Agent 上下文：viewer 分析存档 + 图只读面 + 调查服务。
pub struct AudienceAgentCtx {
    pub viewer_analyses: Map<String, Value>,
    pub research: ResearchService,
    pub store: Store,
    pub graph_run_id: Option<String>,
    pub slot: SubmissionSlot,
}

impl RunCtx for AudienceAgentCtx {
    fn slot(&mut self) -> &mut SubmissionSlot {
        &mut self.slot
    }
}

/// 具备检索服务（search/video 工具共用）。
pub trait HasResearch: RunCtx {
    fn research(&mut self) -> &mut ResearchService;
}

/// 具备图只读面（entity / query_graph 工具共用）。
pub trait HasStore: RunCtx {
    fn store(&self) -> &Store;
}

/// Audience 面：viewer 分析存档 + situation 运行 id。
pub trait HasAudience: HasStore {
    fn analyses(&self) -> &Map<String, Value>;
    fn graph_run_id(&self) -> Option<&str>;
}

impl HasResearch for ViewerAgentCtx {
    fn research(&mut self) -> &mut ResearchService {
        &mut self.research
    }
}
impl HasResearch for AudienceAgentCtx {
    fn research(&mut self) -> &mut ResearchService {
        &mut self.research
    }
}
impl HasStore for ViewerAgentCtx {
    fn store(&self) -> &Store {
        &self.store
    }
}
impl HasStore for AudienceAgentCtx {
    fn store(&self) -> &Store {
        &self.store
    }
}
impl HasAudience for AudienceAgentCtx {
    fn analyses(&self) -> &Map<String, Value> {
        &self.viewer_analyses
    }
    fn graph_run_id(&self) -> Option<&str> {
        self.graph_run_id.as_deref()
    }
}

// ---------------------------------------------------------------------------
// 调查工具（只读；参数 JSON 手部取值 = Python 签名默认值逐一对齐）
// ---------------------------------------------------------------------------

fn obj_schema(fields: &[(&str, Value)], required: &[&str]) -> Value {
    let properties: Map<String, Value> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": required,
    })
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn arg_i64(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

fn arg_str_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `search_bilibili_videos`：按关键词检索真实视频（返回可引用 search_result_id）。
pub fn search_bilibili_videos_tool<C: HasResearch>() -> AgentTool<C> {
    AgentTool {
        name: "search_bilibili_videos".to_string(),
        description: "按具体关键词搜索B站真实视频，返回可引用的search_result_id。".to_string(),
        parameters: obj_schema(
            &[
                ("query", json!({"type": "string"})),
                ("order", json!({"type": "string", "default": "totalrank"})),
                ("limit", json!({"type": "integer", "default": 10})),
            ],
            &["query"],
        ),
        terminal: false,
        handler: Box::new(|ctx: &mut C, args: &Value| {
            let query = arg_str(args, "query").unwrap_or_default();
            let order = arg_str(args, "order").unwrap_or_else(|| "totalrank".to_string());
            let limit = arg_i64(args, "limit", SEARCH_DEFAULT_LIMIT);
            match ctx.research().search(&query, &order, limit) {
                Ok(rows) => json!({
                    "query": query,
                    "count": rows.len(),
                    "items": rows,
                }),
                Err(err) => json!({
                    "query": query,
                    "error": err.to_string(),
                    "items": [],
                }),
            }
        }),
    }
}

/// `get_bilibili_video`：视频详情 + 分区 + TAG（带缓存）。
pub fn get_bilibili_video_tool<C: HasResearch>() -> AgentTool<C> {
    AgentTool {
        name: "get_bilibili_video".to_string(),
        description: "读取真实B站视频详情、当前分区和公开TAG。".to_string(),
        parameters: obj_schema(&[("bvid", json!({"type": "string"}))], &["bvid"]),
        terminal: false,
        handler: Box::new(|ctx: &mut C, args: &Value| {
            let bvid = arg_str(args, "bvid").unwrap_or_default();
            match ctx.research().video(&bvid) {
                Ok(detail) => detail,
                Err(err) => json!({"bvid": bvid, "error": err.to_string()}),
            }
        }),
    }
}

/// `search_entity_candidates`：长期实体注册表检索，用于实体消歧（Viewer 面）。
pub fn search_entity_candidates_tool<C: HasStore>() -> AgentTool<C> {
    AgentTool {
        name: "search_entity_candidates".to_string(),
        description: "在长期实体注册表中搜索名称、别名和类型候选，用于实体消歧。".to_string(),
        parameters: obj_schema(
            &[
                ("query", json!({"type": "string"})),
                ("entity_type", json!({"type": "string", "default": ""})),
                ("limit", json!({"type": "integer", "default": 20})),
            ],
            &["query"],
        ),
        terminal: false,
        handler: Box::new(|ctx: &mut C, args: &Value| {
            let query = arg_str(args, "query").unwrap_or_default();
            let entity_type = arg_str(args, "entity_type").unwrap_or_default();
            let limit = arg_i64(args, "limit", 20).clamp(1, SEARCH_ENTITY_LIMIT_CAP);
            match query::search_entities(ctx.store(), &query, entity_type.trim(), limit) {
                Ok(rows) => json!({
                    "query": query,
                    "entity_type": entity_type,
                    "count": rows.len(),
                    "items": rows,
                }),
                Err(err) => json!({
                    "query": query,
                    "entity_type": entity_type,
                    "error": err.to_string(),
                    "items": [],
                }),
            }
        }),
    }
}

/// `get_viewer_analysis`：读取规范 Viewer Submission；附带 ≤10 条不可变 Episode。
pub fn get_viewer_analysis_tool<C: HasAudience>() -> AgentTool<C> {
    AgentTool {
        name: "get_viewer_analysis".to_string(),
        description: "读取规范Viewer Submission；可按需附带最多10条不可变Episode。".to_string(),
        parameters: obj_schema(
            &[
                ("viewer_id", json!({"type": "string"})),
                (
                    "include_episodes",
                    json!({"type": "boolean", "default": false}),
                ),
                ("episode_limit", json!({"type": "integer", "default": 3})),
            ],
            &["viewer_id"],
        ),
        terminal: false,
        handler: Box::new(|ctx: &mut C, args: &Value| {
            let viewer_id = arg_str(args, "viewer_id").unwrap_or_default();
            let Some(value) = ctx.analyses().get(&viewer_id).cloned() else {
                return json!({"error": "viewer not found", "viewer_id": viewer_id});
            };
            if args
                .get("include_episodes")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let limit = arg_i64(args, "episode_limit", 3).min(VIEWER_EPISODE_LIMIT_CAP);
                match query::episodes(ctx.store(), &viewer_id, Some(limit)) {
                    Ok(episodes) => {
                        let mut merged = value;
                        merged["episodes"] = Value::Array(episodes);
                        merged
                    }
                    Err(err) => json!({"error": err.to_string(), "viewer_id": viewer_id}),
                }
            } else {
                value
            }
        }),
    }
}

/// `query_graph`：按名称/属性/类型/关系检索时序图与 Mention 证据（Audience 面）。
pub fn query_graph_tool<C: HasAudience>() -> AgentTool<C> {
    AgentTool {
        name: "query_graph".to_string(),
        description: "按名称、属性、节点类型和关系检索当前时序图及其Mention证据。".to_string(),
        parameters: obj_schema(
            &[
                ("query", json!({"type": "string"})),
                (
                    "node_types",
                    json!({"type": "array", "items": {"type": "string"}}),
                ),
                (
                    "predicates",
                    json!({"type": "array", "items": {"type": "string"}}),
                ),
                ("limit", json!({"type": "integer", "default": 500})),
            ],
            &["query"],
        ),
        terminal: false,
        handler: Box::new(|ctx: &mut C, args: &Value| {
            let needle = arg_str(args, "query").unwrap_or_default();
            let node_types = arg_str_list(args, "node_types");
            let predicates = arg_str_list(args, "predicates");
            let limit = arg_i64(args, "limit", QUERY_GRAPH_DEFAULT_LIMIT);
            match query::query(
                ctx.store(),
                &needle,
                &node_types,
                &predicates,
                limit,
                ctx.graph_run_id(),
            ) {
                Ok(result) => result,
                Err(err) => json!({"error": err.to_string()}),
            }
        }),
    }
}

/// Viewer Agent 调查工具集（终局工具在 M3-C 由 validators 装配）。
pub fn viewer_investigation_tools() -> Vec<AgentTool<ViewerAgentCtx>> {
    vec![
        search_entity_candidates_tool(),
        search_bilibili_videos_tool(),
        get_bilibili_video_tool(),
    ]
}

/// Audience Agent 调查工具集。
pub fn audience_investigation_tools() -> Vec<AgentTool<AudienceAgentCtx>> {
    vec![
        search_bilibili_videos_tool(),
        get_bilibili_video_tool(),
        get_viewer_analysis_tool(),
        query_graph_tool(),
    ]
}

/// 列出某 result_id 集合的注册表交集（validators 用——修复6：仅本运行注册表可作引用闭包）。
pub fn known_search_result_ids(research: &ResearchService) -> BTreeSet<String> {
    research.search_results.keys().cloned().collect()
}

// ---------------------------------------------------------------------------
// 终局工具厂 + Agent 装配（M3-D；Python submit_* + pipeline.py 组装逐字）
// ---------------------------------------------------------------------------

/// viewer 终局（Python `submit_viewer_perception` 平移：docstring 逐字 + accepted 计数载荷）。
/// schema 校验与 accepted/instruction 协议由 make_terminal_tool 承载；此闭包只做业务校验。
pub fn viewer_terminal_tool() -> AgentTool<ViewerAgentCtx> {
    make_terminal_tool::<ViewerAgentCtx, ViewerPerceptionSubmission, _>(
        "submit_viewer_perception",
        "提交个人Episode抽取、实体消歧、语义关系、兴趣状态和行动；这是唯一有效终局。",
        |ctx, submission| {
            // Python `str(viewer_data.get("viewer", {}).get("id") or "")`
            let viewer_id = py_or_empty(
                ctx.viewer_data
                    .get("viewer")
                    .and_then(|viewer| viewer.get("id")),
            );
            let search_ids: HashSet<String> =
                known_search_result_ids(&ctx.research).into_iter().collect();
            let entity_exists =
                |candidate_id: &str| ctx.store.entity_exists(candidate_id).unwrap_or(false);
            let errors = validate_viewer_submission(
                submission,
                &viewer_id,
                &ctx.episodes,
                &entity_exists,
                &search_ids,
            );
            if errors.is_empty() {
                Ok(json!({
                    "viewer_id": submission.viewer_id,
                    "mentions": submission.mentions.len(),
                    "entities": submission.entities.len(),
                    "relations": submission.relations.len(),
                    "interest_states": submission.interest_states.len(),
                }))
            } else {
                Err(errors)
            }
        },
    )
}

/// audience 终局（Python `submit_audience_situation` 平移：_graph_references 收集口径逐字）。
pub fn audience_terminal_tool() -> AgentTool<AudienceAgentCtx> {
    make_terminal_tool::<AudienceAgentCtx, AudienceSituationSubmission, _>(
        "submit_audience_situation",
        "提交整体兴趣图、观众社区、Situation和内容行动；这是唯一有效终局。",
        |ctx, submission| {
            // entity 引用闭包：interest_graph.entity_id + communities/situations.entity_ids
            // + content_opportunities.entity_id（空串不查；修复6b 的拒收在 validators 层）
            let mut entity_refs: Vec<String> = Vec::new();
            for item in &submission.interest_graph {
                if !item.entity_id.is_empty() {
                    entity_refs.push(item.entity_id.clone());
                }
            }
            for ids in submission
                .communities
                .iter()
                .map(|item| &item.entity_ids)
                .chain(submission.situations.iter().map(|item| &item.entity_ids))
            {
                entity_refs.extend(ids.iter().cloned());
            }
            for item in &submission.content_opportunities {
                if !item.entity_id.is_empty() {
                    entity_refs.push(item.entity_id.clone());
                }
            }
            let mut mention_refs: Vec<String> = Vec::new();
            for mentions in submission
                .interest_graph
                .iter()
                .map(|item| &item.evidence_mention_ids)
                .chain(
                    submission
                        .communities
                        .iter()
                        .map(|item| &item.evidence_mention_ids),
                )
                .chain(
                    submission
                        .situations
                        .iter()
                        .map(|item| &item.evidence_mention_ids),
                )
                .chain(
                    submission
                        .content_opportunities
                        .iter()
                        .map(|item| &item.evidence_mention_ids),
                )
                .chain(
                    submission
                        .individual_highlights
                        .iter()
                        .map(|item| &item.evidence_mention_ids),
                )
            {
                mention_refs.extend(mentions.iter().cloned());
            }
            let references = query::references(&ctx.store, &entity_refs, &[], &mention_refs)
                .map_err(|err| vec![format!("graph references failed: {err}")])?;
            let to_hashset = |key: &str| -> HashSet<String> {
                references
                    .get(key)
                    .map(|set| set.iter().cloned().collect())
                    .unwrap_or_default()
            };
            let viewer_ids: HashSet<String> = ctx.viewer_analyses.keys().cloned().collect();
            let search_ids: HashSet<String> =
                known_search_result_ids(&ctx.research).into_iter().collect();
            let errors = validate_audience_submission(
                submission,
                &viewer_ids,
                &to_hashset("entities"),
                &to_hashset("mentions"),
                &search_ids,
            );
            if errors.is_empty() {
                Ok(json!({
                    "interest_items": submission.interest_graph.len(),
                    "communities": submission.communities.len(),
                    "situations": submission.situations.len(),
                    "content_opportunities": submission.content_opportunities.len(),
                }))
            } else {
                Err(errors)
            }
        },
    )
}

/// Viewer Agent 装配（Python pipeline.py:203-209：name/instructions/tools 顺序逐字）。
pub fn viewer_agent_spec(viewer_id: &str, rules: &[String]) -> AgentSpec<ViewerAgentCtx> {
    let mut tools = viewer_investigation_tools();
    tools.push(viewer_terminal_tool());
    AgentSpec {
        name: format!("Viewer Grounded Perception {viewer_id}"),
        instructions: viewer_instructions(rules),
        tools,
    }
}

/// Audience Agent 装配（Python pipeline.py:320-330）。
pub fn audience_agent_spec(rules: &[String]) -> AgentSpec<AudienceAgentCtx> {
    let mut tools = audience_investigation_tools();
    tools.push(audience_terminal_tool());
    AgentSpec {
        name: "Audience Situation Intelligence".to_string(),
        instructions: audience_instructions(rules),
        tools,
    }
}
