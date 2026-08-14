//! 「实体 AI 归并」三层带（删码刀12，2026-08-13：借产业 ER 之形——
//! blocking → 评分 → 裁决三段；Splink/dedupe 引其身过重，借其形）：
//! 1) **自动带（零 LLM）**：规范化名同型碰撞 + 别名碰撞经并查集成簇，
//!    簇目标排序（platform_fact 唯一即正主 > 最高 degree 的 ai）严格碰撞直并；
//! 2) **裁决带（LLM 只判中间带）**：同型对 (uni,bi)-gram Jaccard ≥0.55
//!    成候选（每实体 top-8、全局 ≤60），prompt 携带清单逐对判——不再全表漫游
//!    （旧制 36 轮 61 条上下文烧穿在案）；候选为空则整轮零 LLM 调用（成本官规）；
//! 3) **drop 通道保留**：清单内垃圾 AI 实体照旧可删。
//!
//! 程序负责事实面（一字未动）：
//! - 全计划校验（实体存在 / 非空 / 全域不重叠 / rationale 长度 / drop 只限 AI 归属）；
//! - 每带各一次外层 maintenance run 记账，逐条执行 entity_merge / entity_drop，
//!   任一失败整体 fail_run + Err（调用方响铃不绊管线）。
//!
//! 分工纪律（AGENTS §2.1）：AI 只裁决「谁归并、谁删除」；真实 ID、存在性、
//! 源类型、时间戳、事务边界全在程序侧。自动带 = 程序内部的确定性可见真理，
//! 不问 AI（官规 2026-08-13 只自动并「严格碰撞」）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Config;
use crate::graph::query;
use crate::graph::store::{MAINTENANCE_RUN_MODEL, Store};

use super::runtime::{
    AgentRuntime, AgentRuntimeError, AgentSpec, AgentTool, AttemptPlan, RunCtx, SubmissionSlot,
    TerminalOutcome, Trace, make_terminal_tool, run_toolcall_agent,
};
use super::tools::HasStore;

/// 一条 merge 动作：把一组碎片（source_entity_ids）的全部边与别名迁移到
/// target_entity_id（最完整的那个实体）。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityMergeAction {
    pub target_entity_id: String,
    pub source_entity_ids: Vec<String>,
    pub rationale: String,
}

/// 一条 drop 动作：整货删除一个「只展示不改事实」的 AI 归属实体。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityDropAction {
    pub entity_id: String,
    pub rationale: String,
}

/// 实体归并终稿（唯一终局 Tool Call 的 submission 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityReconcileDraft {
    #[serde(default)]
    pub merges: Vec<EntityMergeAction>,
    #[serde(default)]
    pub drops: Vec<EntityDropAction>,
}

/// 归并一轮的备报（成功路径；失败直接 Err 不返回备报）。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct EntityReconcileReport {
    /// 自动带：严格碰撞直并的**组数**（零 LLM）。
    pub auto_merged_groups: usize,
    /// 自动带：被并掉的碎片实体总数。
    pub auto_absorbed: usize,
    /// 裁决带：LLM 提交并经过程序校验的计划（候选为空时整带跳过 = 零 LLM）。
    pub planned: EntityReconcileDraft,
    pub merged_ok: usize,
    pub dropped_ok: usize,
    pub failed: Vec<String>,
}

/// rationale 上限（字符）。依据：一句说得清「为什么非动不可」。
pub const RECONCILE_RATIONALE_MAX_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// 三层带预筛（确定性，零 LLM）——名册装载 / n-gram 评分 / 碰撞聚簇 / 候选清单
// ---------------------------------------------------------------------------

/// 候选对进入裁决带的相似度下闸（(uni,bi)-gram Jaccard）。
/// 0.45 定标：CJK 短名 bigram 主导下挫快——「周年纪念装扮主题/周年
/// 纪念活动装扮」实测 0.50（应入带），「异环/环世界」0.14（安稳出局）。
const CANDIDATE_JACCARD_FLOOR: f64 = 0.45;
/// 每实体最多携带的候选配偶数（防高碰撞实体独霸清单）。
const CANDIDATE_PER_ENTITY_CAP: usize = 8;
/// 全局候选对上限（判词预算的硬顶）。
const CANDIDATE_GLOBAL_CAP: usize = 60;

/// 预筛名册行——三层带唯一取数面（背靠 list_entities 投影，含 source_kind/degree）。
#[derive(Debug, Clone)]
struct RosterRow {
    entity_id: String,
    canonical_name: String,
    entity_type: String,
    source_kind: String,
    degree: i64,
    aliases: Vec<String>,
}

fn load_roster(store: &Store) -> Vec<RosterRow> {
    let mut rows = Vec::new();
    let mut offset: i64 = 0;
    loop {
        let page = query::list_entities(store, offset, query::LIST_ENTITIES_LIMIT)
            .unwrap_or_else(|_| json!({"items": []}));
        let items = page["items"].as_array().cloned().unwrap_or_default();
        let reached_end = (items.len() as i64) < query::LIST_ENTITIES_LIMIT;
        for item in items {
            rows.push(RosterRow {
                entity_id: item["entity_id"].as_str().unwrap_or_default().to_string(),
                canonical_name: item["canonical_name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                entity_type: item["entity_type"].as_str().unwrap_or_default().to_string(),
                source_kind: item["source_kind"].as_str().unwrap_or_default().to_string(),
                degree: item["degree"].as_i64().unwrap_or(0),
                aliases: item["aliases"]
                    .as_array()
                    .map(|aliases| {
                        aliases
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
        if reached_end {
            break;
        }
        offset += query::LIST_ENTITIES_LIMIT;
    }
    rows
}

/// CJK 短名评分：(uni,bi)-gram 集合。中文无空格分词脐带，字元 n-gram 是产业 ER
/// 对短名的标准解；规范化（小写/去分隔符）与 norm() 同尺——嵌入件留作升舱路。
fn name_ngrams(text: &str) -> std::collections::BTreeSet<String> {
    let chars: Vec<char> = crate::episodes::norm(text).chars().collect();
    let mut grams = std::collections::BTreeSet::new();
    for ch in &chars {
        grams.insert(ch.to_string());
    }
    for pair in chars.windows(2) {
        grams.insert(pair.iter().collect());
    }
    grams
}

fn ngram_jaccard(a: &str, b: &str) -> f64 {
    let ga = name_ngrams(a);
    let gb = name_ngrams(b);
    if ga.is_empty() || gb.is_empty() {
        return 0.0;
    }
    let inter = ga.intersection(&gb).count() as f64;
    inter / ga.union(&gb).count() as f64
}

/// 一侧的变体面 = canonical ∪ aliases（别名空间是碎片同名的高发区）。
fn side_variants(row: &RosterRow) -> Vec<&str> {
    std::iter::once(row.canonical_name.as_str())
        .chain(row.aliases.iter().map(String::as_str))
        .collect()
}

fn pair_score(a: &RosterRow, b: &RosterRow) -> f64 {
    let mut best = 0.0_f64;
    for va in side_variants(a) {
        for vb in side_variants(b) {
            best = best.max(ngram_jaccard(va, vb));
        }
    }
    best
}

/// 迷你并查集——碰撞聚簇与（潜在）链式碰撞的连通件。
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            let root = self.find(self.parent[x]);
            self.parent[x] = root;
        }
        self.parent[x]
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }

    /// 根 → 成员下标集（只留 >1 的簇；空键串 normalized 不连边——空名不配碰撞）。
    fn clusters(&mut self, n: usize) -> Vec<Vec<usize>> {
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            groups.entry(self.find(i)).or_default().push(i);
        }
        groups
            .into_values()
            .filter(|group| group.len() > 1)
            .collect()
    }
}

/// 自动带：严格碰撞簇直并。返回（merge 动作列， 被吸收碎片数）。
/// 目标排序：簇内恰一个 platform_fact → 它是正主；否则最高 degree 的 ai
/// （degree 并列取 entity_id 字节序小者——确定性）。簇内含 ≥2 个 platform_fact、
/// 或任一非目标成员非 ai → 整簇跳过（事实闸绝不绕——异环 game↔bilibili_tag
/// 那类跨源碰撞留给裁决带在相似度评分下自然浮现）。
fn auto_band(roster: &[RosterRow]) -> (Vec<EntityMergeAction>, usize) {
    let n = roster.len();
    let mut uf = UnionFind::new(n);
    // 键①：规范化名碰撞组（不限型——type 在本星是取证来源道，不是本体：
    // 异环(game-AI 道) vs 异环(bilibili_tag 平台道)恰是自动带最高价猎物；
    // 「严格碰撞」的官规语义落在规范化字符串全等，不落类型同源）。
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in roster.iter().enumerate() {
        let key = crate::episodes::norm(&row.canonical_name);
        if !key.is_empty() {
            by_name.entry(key).or_default().push(index);
        }
    }
    for group in by_name.values() {
        for pair in group.windows(2) {
            uf.union(pair[0], pair[1]);
        }
    }
    // 键②：别名碰撞——alias_key（=norm(alias)，含 canonical 自身落别名表的口径）
    // 指向 ≥2 实体时连边；跨型别名碰撞不连（连了也过不了簇审查，省一趟）。
    let mut by_alias: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in roster.iter().enumerate() {
        for alias in side_variants(row) {
            let key = crate::episodes::norm(alias);
            if !key.is_empty() {
                by_alias.entry(key).or_default().push(index);
            }
        }
    }
    for group in by_alias.values().filter(|g| g.len() > 1) {
        for pair in group.windows(2) {
            if roster[pair[0]].entity_type == roster[pair[1]].entity_type {
                uf.union(pair[0], pair[1]);
            }
        }
    }
    let mut merges = Vec::new();
    let mut absorbed = 0usize;
    for cluster in uf.clusters(n) {
        let members: Vec<&RosterRow> = cluster.iter().map(|&i| &roster[i]).collect();
        let facts: Vec<&&RosterRow> = members
            .iter()
            .filter(|row| row.source_kind != "ai")
            .collect();
        let target: &RosterRow = match facts.len() {
            1 => facts[0],
            0 => members
                .iter()
                .max_by(|x, y| {
                    x.degree
                        .cmp(&y.degree)
                        .then_with(|| y.entity_id.cmp(&x.entity_id))
                })
                .expect("簇非空"),
            // ≥2 平台事实互碰：结构性不可解（谁都不能当源）——移交（v1 略过）。
            _ => continue,
        };
        let sources: Vec<String> = members
            .iter()
            .filter(|row| row.entity_id != target.entity_id)
            .map(|row| row.entity_id.clone())
            .collect();
        if sources.is_empty()
            || members
                .iter()
                .any(|row| row.entity_id != target.entity_id && row.source_kind != "ai")
        {
            continue;
        }
        absorbed += sources.len();
        let names: Vec<&str> = members
            .iter()
            .map(|row| row.canonical_name.as_str())
            .collect();
        merges.push(EntityMergeAction {
            target_entity_id: target.entity_id.clone(),
            source_entity_ids: sources,
            rationale: format!("规范化名/别名严格碰撞自动归并（{}）", names.join("、")),
        });
    }
    (merges, absorbed)
}

/// 候选对（裁决带的输入单元）。
#[derive(Debug, Clone)]
struct CandidatePair {
    a: RosterRow,
    b: RosterRow,
    score: f64,
}

/// 裁决带候选清单：同型 + 至少一端 ai 可移（双平台事实结构性不可解，不浪费判词）+
/// Jaccard ≥ 下闸；每实体 top-8 截流，全局无序对去重后按分降序截 60。
fn candidate_band(roster: &[RosterRow]) -> Vec<CandidatePair> {
    let mut per_entity: HashMap<usize, Vec<(usize, f64)>> = HashMap::new();
    for i in 0..roster.len() {
        for j in (i + 1)..roster.len() {
            let (a, b) = (&roster[i], &roster[j]);
            if a.entity_type != b.entity_type {
                continue;
            }
            if a.source_kind != "ai" && b.source_kind != "ai" {
                continue;
            }
            let score = pair_score(a, b);
            if score < CANDIDATE_JACCARD_FLOOR {
                continue;
            }
            per_entity.entry(i).or_default().push((j, score));
            per_entity.entry(j).or_default().push((i, score));
        }
    }
    let mut seen: std::collections::BTreeSet<(usize, usize)> = std::collections::BTreeSet::new();
    let mut pairs: Vec<CandidatePair> = Vec::new();
    for (i, mut partners) in per_entity {
        partners.sort_by(|x, y| y.1.total_cmp(&x.1));
        partners.truncate(CANDIDATE_PER_ENTITY_CAP);
        for (j, score) in partners {
            let key = (i.min(j), i.max(j));
            if seen.insert(key) {
                let (a, b) = if i <= j {
                    (&roster[i], &roster[j])
                } else {
                    (&roster[j], &roster[i])
                };
                pairs.push(CandidatePair {
                    a: a.clone(),
                    b: b.clone(),
                    score,
                });
            }
        }
    }
    pairs.sort_by(|x, y| y.score.total_cmp(&x.score));
    pairs.truncate(CANDIDATE_GLOBAL_CAP);
    pairs
}

// ---------------------------------------------------------------------------
// ctx：工具处理器跑在独立 scoped thread 上，需要自持 repository（不能被 & 借用）
// ---------------------------------------------------------------------------

pub struct ReconcileContext {
    pub store: Store,
    pub submission: Option<EntityReconcileDraft>,
    pub slot: SubmissionSlot,
}

impl RunCtx for ReconcileContext {
    fn slot(&mut self) -> &mut SubmissionSlot {
        &mut self.slot
    }
}

impl HasStore for ReconcileContext {
    fn store(&self) -> &Store {
        &self.store
    }
}

// ---------------------------------------------------------------------------
// 只读调查工具 + 唯一终局工具
// ---------------------------------------------------------------------------

fn obj_schema(fields: &[(&str, Value)], required: &[&str]) -> Value {
    let properties: HashMap<String, Value> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn arg_i64(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

/// `list_entities`：分页列出实体注册表全量（只读调查）。
pub fn list_entities_tool<C: HasStore>() -> AgentTool<C> {
    AgentTool {
        name: "list_entities".to_string(),
        description: "分页列出长期实体注册表中的全部实体（按实体 ID 稳定排序，只读）。".to_string(),
        parameters: obj_schema(
            &[
                ("offset", json!({"type": "integer", "default": 0})),
                ("limit", json!({"type": "integer", "default": 100})),
            ],
            &[],
        ),
        terminal: false,
        handler: Box::new(|ctx: &mut C, args: &Value| {
            let offset = arg_i64(args, "offset", 0);
            let limit = arg_i64(args, "limit", 100);
            match query::list_entities(ctx.store(), offset, limit) {
                Ok(value) => value,
                Err(err) => json!({"error": err.to_string(), "count": 0, "items": []}),
            }
        }),
    }
}

/// `search_entities`：按名称/别名/类型搜索候选实体（只读）。
pub fn search_entities_tool<C: HasStore>() -> AgentTool<C> {
    AgentTool {
        name: "search_entities".to_string(),
        description: "按名称、别名或类型在长期实体注册表中搜索候选（只读）。".to_string(),
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
            let keyword = arg_str(args, "query").unwrap_or_default();
            let entity_type = arg_str(args, "entity_type").unwrap_or_default();
            let limit = arg_i64(args, "limit", 20).clamp(1, 100);
            match query::search_entities(ctx.store(), &keyword, entity_type.trim(), limit) {
                Ok(rows) => json!({
                    "query": keyword,
                    "entity_type": entity_type,
                    "count": rows.len(),
                    "items": rows,
                }),
                Err(err) => json!({
                    "query": keyword,
                    "entity_type": entity_type,
                    "count": 0,
                    "error": err.to_string(),
                    "items": [],
                }),
            }
        }),
    }
}

/// 校验 Rule 概述：全部实体 ID 必须存在；merge target 不得出现在自身 sources；
/// 全计划任何实体 ID 只能被使用一次；rationale 非空且 ≤200 字；drop 与 **merge 源**
/// 只允许 source_kind == 'ai' 的实体（bilibili_tag/bilibili_category/creator 等平台
/// 事实是 UI 事实面，不是 AI 可删面——merge 会实删源实体，故平台事实只可当被并入
/// 方 target）。空计划恒合法（= 不确定就不动）。
fn validate_draft(store: &Store, draft: &EntityReconcileDraft) -> Vec<String> {
    let mut errors = Vec::new();
    if draft.merges.is_empty() && draft.drops.is_empty() {
        return errors;
    }

    let source_kind_of = |id: &str| -> Option<String> {
        store
            .conn
            .query_row(
                "SELECT source_kind FROM entities WHERE entity_id=?1",
                [id],
                |row| row.get(0),
            )
            .ok()
    };

    let check_rationale = |label: &str, rationale: &str, errors: &mut Vec<String>| {
        let trimmed = rationale.trim();
        if trimmed.is_empty() {
            errors.push(format!("{label}.rationale 不得为空"));
        }
        let len = trimmed.chars().count();
        if len > RECONCILE_RATIONALE_MAX_CHARS {
            errors.push(format!(
                "{label}.rationale 超长：{len} > {RECONCILE_RATIONALE_MAX_CHARS} 字"
            ));
        }
    };

    // 全计划 ID 占用表：实体只能出现在一个动作里（含 merge target/source/drop）。
    let mut owner: HashMap<String, String> = HashMap::new();
    let mut claim = |id: &str, label: &str, errors: &mut Vec<String>| {
        if id.is_empty() {
            return;
        }
        if let Some(prev) = owner.insert(id.to_string(), label.to_string()) {
            errors.push(format!(
                "实体 {id} 在多个动作里出现：{prev} 与 {label} 重复使用"
            ));
        }
    };

    for (i, action) in draft.merges.iter().enumerate() {
        let label = format!("merges[{}]", i);
        let target = action.target_entity_id.trim();
        if target.is_empty() {
            errors.push(format!("{label}.target_entity_id 不能为空"));
        } else if !store.entity_exists(target).unwrap_or(false) {
            errors.push(format!("{label}.target_entity_id 不存在：{target}"));
        }
        if action.source_entity_ids.is_empty() {
            errors.push(format!(
                "{label}.source_entity_ids 不能为空（至少一个碎片实体）"
            ));
        }
        let mut seen_sources: Vec<String> = Vec::new();
        for (j, raw) in action.source_entity_ids.iter().enumerate() {
            let source = raw.trim();
            if source.is_empty() {
                errors.push(format!("{label}.source_entity_ids[{j}] 不能为空"));
                continue;
            }
            if seen_sources.contains(&source.to_string()) {
                errors.push(format!(
                    "{label}.source_entity_ids[{j}] 与本 merge 内重复：{source}"
                ));
            } else {
                seen_sources.push(source.to_string());
            }
            if source == target {
                errors.push(format!(
                    "{label}：target 与 source 重叠（self-merge）：{source}"
                ));
            }
            if !store.entity_exists(source).unwrap_or(false) {
                errors.push(format!("{label}.source_entity_ids[{j}] 不存在：{source}"));
            }
            if let Some(kind) = source_kind_of(source)
                && kind != "ai"
            {
                errors.push(format!(
                    "{label}.source_entity_ids[{j}]：实体 {source} 的 source_kind={kind}，\
                     merge 会实删源实体——平台事实只可当被并入方（target），不可作 merge 源"
                ));
            }
            claim(
                source,
                &format!("{label}.source_entity_ids[{j}]"),
                &mut errors,
            );
        }
        if !target.is_empty() {
            claim(target, &format!("{label}.target_entity_id"), &mut errors);
        }
        check_rationale(&label, &action.rationale, &mut errors);
    }

    for (i, action) in draft.drops.iter().enumerate() {
        let label = format!("drops[{}]", i);
        let id = action.entity_id.trim();
        if id.is_empty() {
            errors.push(format!("{label}.entity_id 不能为空"));
            continue;
        }
        if !store.entity_exists(id).unwrap_or(false) {
            errors.push(format!("{label}.entity_id 不存在：{id}"));
        }
        if let Some(kind) = source_kind_of(id)
            && kind != "ai"
        {
            errors.push(format!(
                "{label}：实体 {id} 的 source_kind={kind}，平台事实/托管数据不可删除（仅 AI 归属可删）"
            ));
        }
        claim(id, &format!("{label}.entity_id"), &mut errors);
        check_rationale(&label, &action.rationale, &mut errors);
    }

    errors
}

/// 归并 Agent 的工具面：两个只读调查 + 唯一终局。
pub fn reconcile_tools() -> Vec<AgentTool<ReconcileContext>> {
    vec![
        list_entities_tool(),
        search_entities_tool(),
        make_terminal_tool(
            "submit_entity_reconcile",
            "提交实体归并计划（merges=把碎片合并回同一实体；drops=删除纯展示噪音的AI归属实体）。\
             这是唯一有效终局：不确定就提交可接受空计划。",
            |ctx: &mut ReconcileContext, draft: &EntityReconcileDraft| {
                let errors = validate_draft(ctx.store(), draft);
                if errors.is_empty() {
                    ctx.submission = Some(draft.clone());
                    TerminalOutcome::Accept(
                        json!({"accepted": true, "merged": draft.merges.len(), "dropped": draft.drops.len()}),
                    )
                } else {
                    TerminalOutcome::Reject(errors)
                }
            },
        ),
    ]
}

fn reconcile_instructions() -> String {
    "你是实体图谱的归并维护助手。长期注册表里，观众逐轮提交会把同一个作品/角色/概念拆成多个碎片，\
    也会留下只有装饰意义的外壳实体。程序已按名称/别名相似度预筛出候选分身对（见用户消息），\
    同键严格碰撞的碎渣程序已自动并掉、不用你管。你的任务：\
    1) 逐对审查候选清单——拿不准的对可用 list_entities / search_entities 调查\
       （返回含 source_kind 与 degree，可直接引用）；\
    2) 只把证据确凿指向同一真实对象的合并：merge 的 target_entity_id 必须是证据最全、\
       最完整的一方（清单两端的 degree 与来源可作重要参考），其余进 source_entity_ids；\
    3) drop：只删名单内确为噪音的实体，且只允许 source_kind='ai' 的 AI 归属实体；\
       bilibili_tag / category / creator 等平台事实实体绝不能删除，也不应作 merge 源；\
    4) 每个动作的 rationale 必须给出依据（非空、≤200 字）；\
    5) 拿不准的对就跳过；整单都不动则提交空计划。\
    纪律：你只决定「谁归并、谁删除」，ID、存在性、时间等由程序校验。普通文本不是有效输出，\
    必须调用 submit_entity_reconcile 提交你的计划。"
        .to_string()
}

fn reconcile_user_prompt(total: usize, pairs: &[CandidatePair]) -> String {
    let mut text = format!(
        "当前长期实体注册表共 {total} 个实体；程序预筛出候选分身对 {} 对（按相似度降序）。请逐对裁决：\n",
        pairs.len()
    );
    for (index, pair) in pairs.iter().enumerate() {
        let alias_of = |row: &RosterRow| {
            if row.aliases.is_empty() {
                "-".to_string()
            } else {
                row.aliases.join(" | ")
            }
        };
        text.push_str(&format!(
            "{}) {} [{}]（{}, {}, degree={}, 别名:{}）⇄ {} [{}]（{}, {}, degree={}, 别名:{}）\n",
            index + 1,
            pair.a.canonical_name,
            pair.a.entity_id,
            pair.a.entity_type,
            pair.a.source_kind,
            pair.a.degree,
            alias_of(&pair.a),
            pair.b.canonical_name,
            pair.b.entity_id,
            pair.b.entity_type,
            pair.b.source_kind,
            pair.b.degree,
            alias_of(&pair.b),
        ));
    }
    text.push_str("清单之外若查得其他硬证据碎片，也可并案提交。");
    text
}

/// 程序侧事实层：一次外层 maintenance run 记账，逐笔执行一「带」的裁决。
/// 空计划不记账（见下方 run_entity_reconcile 的对照窗注释）。
/// Err = 任一动作失败（该带 fail_run + 全盘 Err）。
fn execute_band(
    store: &Store,
    draft: &EntityReconcileDraft,
) -> Result<(usize, usize), AgentRuntimeError> {
    let detail_json = serde_json::to_string(draft).ok();
    let run_id = format!("run:{}", uuid::Uuid::new_v4().simple());
    let now = crate::episodes::now_iso();
    store
        .begin_run_typed(
            &run_id,
            &now,
            MAINTENANCE_RUN_MODEL,
            Store::RUN_KIND_MAINTENANCE,
            detail_json.as_deref(),
        )
        .map_err(|err| AgentRuntimeError::Protocol(format!("reconcile 开账失败：{err}")))?;

    let mut merged_ok = 0usize;
    let mut dropped_ok = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for action in &draft.merges {
        match store.entity_merge(&action.source_entity_ids, &action.target_entity_id) {
            Ok(_) => merged_ok += 1,
            Err(err) => failed.push(format!(
                "merge {} <- {:?}：{err}",
                action.target_entity_id, action.source_entity_ids
            )),
        }
    }
    for action in &draft.drops {
        match store.entity_drop(&action.entity_id) {
            Ok(()) => dropped_ok += 1,
            Err(err) => failed.push(format!("drop {}：{err}", action.entity_id)),
        }
    }

    if !failed.is_empty() {
        let joined = failed.join("；");
        let _ = store.fail_run(&run_id, &joined, false);
        return Err(AgentRuntimeError::Protocol(format!(
            "实体归并未达成：{joined}"
        )));
    }
    store
        .complete_run(&run_id)
        .map_err(|err| AgentRuntimeError::Protocol(format!("reconcile 记账失败：{err}")))?;
    Ok((merged_ok, dropped_ok))
}

/// 跑一轮实体归并（三层带：自动带直并 → 裁决带喂候选 → 各有账各执行）。
/// Err = 协议/网络/拒绝/部分失败——调用方响铃并把归并留空。
pub async fn run_entity_reconcile(
    runtime: &AgentRuntime,
    config: &Config,
    store: &Store,
) -> Result<EntityReconcileReport, AgentRuntimeError> {
    // 带①自动带：严格碰撞直并（零 LLM）。
    let roster = load_roster(store);
    let (auto_merges, auto_absorbed) = auto_band(&roster);
    let auto_merged_groups = auto_merges.len();
    if !auto_merges.is_empty() {
        execute_band(
            store,
            &EntityReconcileDraft {
                merges: auto_merges,
                drops: Vec::new(),
            },
        )?;
    }

    // 带②裁决带：碰撞并重载名册后取候选；空 = 整带跳过（零 LLM，成本官规）。
    let roster = load_roster(store);
    let pairs = candidate_band(&roster);
    let mut report = EntityReconcileReport {
        auto_merged_groups,
        auto_absorbed,
        planned: EntityReconcileDraft {
            merges: Vec::new(),
            drops: Vec::new(),
        },
        merged_ok: 0,
        dropped_ok: 0,
        failed: Vec::new(),
    };
    if pairs.is_empty() {
        return Ok(report);
    }

    let mut ctx = ReconcileContext {
        store: Store::open(&config.output_dir.join("graph").join("perception.sqlite3"))
            .map_err(|err| AgentRuntimeError::Protocol(format!("reconcile 打开图库失败：{err}")))?,
        submission: None,
        slot: SubmissionSlot::default(),
    };
    let mut spec = AgentSpec {
        name: "entity-reconcile".to_string(),
        instructions: reconcile_instructions(),
        tools: reconcile_tools(),
    };
    let trace_path = config
        .ai
        .agent
        .local_trace
        .then(|| config.output_dir.join("ai/traces/entity-reconcile.jsonl"));
    let mut trace = Trace::new(trace_path);
    let prompt = reconcile_user_prompt(roster.len(), &pairs);
    let outcome = run_toolcall_agent::<ReconcileContext, EntityReconcileDraft>(
        runtime,
        &mut spec,
        AttemptPlan {
            label: "entity-reconcile",
            prompt: &prompt,
            // 轮数不设限（2026-08-07 官规）：刹车 = 三级催交阶梯 + token 熔断——
            // 2026-08-13 补漏：熔断真接线（旧注释声称 viewer_token_budget 在岗，
            // 实则 None 放行——situation/run 两段的同款面照pipeline/viewer.rs:189 接法）。
            max_turns: usize::MAX,
            retries: config.ai.agent.run_retries.max(0) as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
            token_budget: Some(config.ai.agent.viewer_token_budget),
        },
        &mut ctx,
        &mut trace,
    )
    .await?;
    report.planned = outcome.submission;

    // 空计划不记账：无动作的 maintenance run 会照常进 run_pair_delta 的
    // 「相邻 complete」对照窗，把「vs 上轮感知」稀释成无变化（同类坑
    // 已用 kind 排除 recap-refresh 钉过一次）——没消费者就不出生。
    if report.planned.merges.is_empty() && report.planned.drops.is_empty() {
        return Ok(report);
    }
    let (merged_ok, dropped_ok) = execute_band(store, &report.planned)?;
    report.merged_ok = merged_ok;
    report.dropped_ok = dropped_ok;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::store::Store;

    const FIXED_NOW: &str = "2026-08-03T07:02:19.191879+00:00";

    fn mem_store() -> Store {
        Store::open_with_clock(Path::new(":memory:"), || FIXED_NOW.to_string()).unwrap()
    }

    fn seed(store: &Store, id: &str, kind: &str) {
        store
            .conn
            .execute(
                "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,\
                 description,source_kind,properties_json,first_seen_at,last_seen_at) \
                 VALUES(?1,?2,?2,'game','d',?3,'{}','t0','t1')",
                [id, id, kind],
            )
            .unwrap();
    }

    fn draft(
        merges: Vec<serde_json::Value>,
        drops: Vec<serde_json::Value>,
    ) -> EntityReconcileDraft {
        serde_json::from_value(json!({
            "merges": serde_json::Value::Array(merges),
            "drops": serde_json::Value::Array(drops),
        }))
        .unwrap()
    }

    #[test]
    fn missing_entity_id_is_rejected() {
        let store = mem_store();
        seed(&store, "e:a1", "ai");
        let d = draft(
            vec![
                json!({"target_entity_id": "e:ghost", "source_entity_ids": ["e:a1"], "rationale": "r"}),
            ],
            vec![],
        );
        let errors = validate_draft(&store, &d);
        assert!(
            errors.iter().any(|e| e.contains("target_entity_id 不存在")),
            "{errors:?}"
        );
    }

    #[test]
    fn self_merge_is_rejected() {
        let store = mem_store();
        seed(&store, "e:a1", "ai");
        let d = draft(
            vec![
                json!({"target_entity_id": "e:a1", "source_entity_ids": ["e:a1"], "rationale": "r"}),
            ],
            vec![],
        );
        let errors = validate_draft(&store, &d);
        assert!(errors.iter().any(|e| e.contains("重叠")), "{errors:?}");
    }

    #[test]
    fn overlapping_id_across_actions_is_rejected() {
        let store = mem_store();
        seed(&store, "e:a1", "ai");
        seed(&store, "e:a2", "ai");
        seed(&store, "e:a3", "ai");
        let d = draft(
            vec![
                json!({"target_entity_id": "e:a1", "source_entity_ids": ["e:a2"], "rationale": "r1"}),
                json!({"target_entity_id": "e:a3", "source_entity_ids": ["e:a2"], "rationale": "r2"}),
            ],
            vec![],
        );
        let errors = validate_draft(&store, &d);
        assert!(errors.iter().any(|e| e.contains("多个动作")), "{errors:?}");
    }

    #[test]
    fn platform_fact_drop_is_rejected() {
        let store = mem_store();
        seed(&store, "e:tag1", "platform_fact");
        let d = draft(
            vec![],
            vec![json!({"entity_id": "e:tag1", "rationale": "噪音"})],
        );
        let errors = validate_draft(&store, &d);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("source_kind=platform_fact")),
            "{errors:?}"
        );
    }

    #[test]
    fn platform_fact_merge_source_is_rejected_target_allowed() {
        let store = mem_store();
        seed(&store, "e:tag1", "platform_fact");
        seed(&store, "e:a1", "ai");
        // 平台事实作 merge 源：并入会实删源实体 → 事实闸拒。
        let d = draft(
            vec![
                json!({"target_entity_id": "e:a1", "source_entity_ids": ["e:tag1"], "rationale": "r"}),
            ],
            vec![],
        );
        let errors = validate_draft(&store, &d);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("source_kind=platform_fact")),
            "{errors:?}"
        );
        // 平台事实作 target（被并入方）：自身行不被改写 → 放行。
        let d = draft(
            vec![
                json!({"target_entity_id": "e:tag1", "source_entity_ids": ["e:a1"], "rationale": "r"}),
            ],
            vec![],
        );
        assert!(validate_draft(&store, &d).is_empty());
    }

    // ------------------------------------------------------------------
    // 三层带预筛钉组（删码刀12）
    // ------------------------------------------------------------------

    fn roster_row(
        id: &str,
        name: &str,
        ty: &str,
        kind: &str,
        degree: i64,
        aliases: &[&str],
    ) -> RosterRow {
        RosterRow {
            entity_id: id.to_string(),
            canonical_name: name.to_string(),
            entity_type: ty.to_string(),
            source_kind: kind.to_string(),
            degree,
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn ngram_jaccard_cjk_semantics() {
        // norm 后同形（大小写/分隔符差异）→ 1.0。
        assert!((ngram_jaccard("鱼SLASH", "鱼_slash") - 1.0).abs() < 1e-9);
        // 异环 vs 环世界：不同作品不得高票（共享单字不构成候选）。
        assert!(ngram_jaccard("异环", "环世界") < CANDIDATE_JACCARD_FLOOR);
        // 中重叠：『周年纪念装扮主题』vs『周年纪念活动装扮』应过闸。
        let mid = ngram_jaccard("周年纪念装扮主题", "周年纪念活动装扮");
        assert!(mid >= CANDIDATE_JACCARD_FLOOR, "{mid}");
        // 空串侧恒 0。
        assert_eq!(ngram_jaccard("", "异环"), 0.0);
    }

    #[test]
    fn auto_band_merges_strict_collisions_platform_fact_becomes_target() {
        let roster = vec![
            roster_row("entity:game:aaa", "异环", "game", "ai", 10, &["环宝"]),
            roster_row(
                "entity:bilibili_tag:bbb",
                "异环",
                "bilibili_tag",
                "platform_fact",
                2,
                &[],
            ),
            roster_row("entity:game:ccc", "环宝", "game", "ai", 3, &[]),
            roster_row("entity:game:ddd", "界环", "game", "ai", 1, &[]),
        ];
        let (merges, absorbed) = auto_band(&roster);
        // 「异环」双源碰撞（同名跨 kind）+「环宝」别名碰撞 → 并查集一簇：
        // 正主 = 唯一 platform_fact（bilibili_tag）。
        assert_eq!(merges.len(), 1, "{merges:?}");
        assert_eq!(absorbed, 2);
        let action = &merges[0];
        assert_eq!(action.target_entity_id, "entity:bilibili_tag:bbb");
        assert!(
            action
                .source_entity_ids
                .contains(&"entity:game:aaa".to_string())
        );
        assert!(
            action
                .source_entity_ids
                .contains(&"entity:game:ccc".to_string())
        );
        assert!(action.rationale.contains("严格碰撞"));
        // 「界环」无碰撞 → 不动。
        assert!(
            !action
                .source_entity_ids
                .contains(&"entity:game:ddd".to_string())
        );
    }

    #[test]
    fn auto_band_two_platform_facts_cluster_is_skipped() {
        // 双平台事实互碰：结构性不可解（谁都不能当源）→ 整簇略过（不移交不硬并）。
        let roster = vec![
            roster_row(
                "entity:bilibili_tag:p1",
                "异环",
                "bilibili_tag",
                "platform_fact",
                9,
                &[],
            ),
            roster_row(
                "entity:category:p2",
                "异环",
                "bilibili_tag",
                "platform_fact",
                8,
                &[],
            ),
        ];
        let (merges, absorbed) = auto_band(&roster);
        assert!(merges.is_empty());
        assert_eq!(absorbed, 0);
    }

    #[test]
    fn auto_band_all_ai_picks_highest_degree_target() {
        let roster = vec![
            roster_row("entity:game:hi", "环宝", "game", "ai", 12, &[]),
            roster_row("entity:game:lo", "环宝", "game", "ai", 4, &[]),
        ];
        let (merges, absorbed) = auto_band(&roster);
        assert_eq!(merges.len(), 1);
        assert_eq!(absorbed, 1);
        assert_eq!(merges[0].target_entity_id, "entity:game:hi");
        assert_eq!(
            merges[0].source_entity_ids,
            vec!["entity:game:lo".to_string()]
        );
    }

    #[test]
    fn candidate_band_bands_middle_and_rejects_structure() {
        let roster = vec![
            // 高相似（别名桥）→ 候选。
            roster_row(
                "entity:game:a1",
                "周年纪念装扮主题",
                "game",
                "ai",
                5,
                &["周年装扮"],
            ),
            roster_row("entity:game:a2", "周年纪念活动装扮", "game", "ai", 2, &[]),
            // 低相似 → 不进带。
            roster_row("entity:game:a3", "异环", "game", "ai", 9, &[]),
            // 双平台事实：相似度再高也结构性开除。
            roster_row(
                "entity:bilibili_tag:t1",
                "星轨铁道",
                "bilibili_tag",
                "platform_fact",
                9,
                &[],
            ),
            roster_row(
                "entity:bilibili_tag:t2",
                "星轨铁道",
                "bilibili_tag",
                "platform_fact",
                8,
                &[],
            ),
            // 跨型相似：不开候选（归并跨型无用途面）。
            roster_row(
                "entity:creator:c1",
                "周年纪念装扮主题",
                "creator",
                "ai",
                1,
                &[],
            ),
        ];
        let pairs = candidate_band(&roster);
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        assert_eq!(
            (pairs[0].a.entity_id.as_str(), pairs[0].b.entity_id.as_str()),
            ("entity:game:a1", "entity:game:a2")
        );
        assert!(pairs[0].score >= CANDIDATE_JACCARD_FLOOR);
    }

    #[test]
    fn candidate_prompt_lists_pairs_with_kind_and_degree() {
        let pairs = vec![CandidatePair {
            a: roster_row("entity:game:a1", "异环副体", "game", "ai", 7, &["环"]),
            b: roster_row(
                "entity:bilibili_tag:t9",
                "异环",
                "bilibili_tag",
                "platform_fact",
                3,
                &[],
            ),
            score: 0.91,
        }];
        let text = reconcile_user_prompt(42, &pairs);
        assert!(text.contains("共 42 个实体"));
        assert!(text.contains("候选分身对 1 对"));
        assert!(text.contains("entity:game:a1"), "{text}");
        assert!(text.contains("platform_fact"), "{text}");
        assert!(text.contains("degree=7"), "{text}");
    }

    #[test]
    fn empty_draft_is_accepted() {
        let store = mem_store();
        seed(&store, "e:a1", "ai");
        let d = draft(vec![], vec![]);
        assert!(validate_draft(&store, &d).is_empty());
    }

    #[test]
    fn valid_plan_and_ai_drop_are_accepted() {
        let store = mem_store();
        seed(&store, "e:keep", "ai");
        seed(&store, "e:drop", "ai");
        let d = draft(
            vec![],
            vec![json!({"entity_id": "e:drop", "rationale": "纯展示空壳"})],
        );
        assert!(validate_draft(&store, &d).is_empty());
    }
}

// （tests 增补见同文件 cfg(test) 模块——三层带钉件组）
