//! R2 批1「实体 AI 归并」（用户裁决：合并不靠规则表——AI 看图裁决，程序出纳）：
//! 只在观看证据入库后，让 AI 用只读调查工具（list_entities / search_entities）
//! 看清当前实体表，再通过唯一终局 Tool Call 提交一份「合并 / 删除」计划。
//! 程序负责事实面：
//! - 全计划校验（实体存在 / 非空 / 全域不重叠 / rationale 长度 / drop 只限 AI 归属）；
//! - 一次外层 maintenance run 记账，逐条执行 entity_merge / entity_drop，
//!   任一失败整体 fail_run + Err（调用方响铃不绊管线）。
//!
//! 分工纪律（AGENTS §2.1）：AI 只裁决「谁归并、谁删除」；真实 ID、存在性、
//! 源类型、时间戳、事务边界全在程序侧。没有固定知识 / 模糊匹配别名魔法。

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
    pub planned: EntityReconcileDraft,
    pub merged_ok: usize,
    pub dropped_ok: usize,
    pub failed: Vec<String>,
}

/// rationale 上限（字符）。依据：一句说得清「为什么非动不可」。
pub const RECONCILE_RATIONALE_MAX_CHARS: usize = 200;

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
    也会留下只有装饰意义的外壳实体。你的任务：\
    1) 用 list_entities / search_entities 只读调查当前实体（优先全量分页读完）；\
    2) 判断哪些碎片指向同一个真实对象——只有证据确凿才归并；\
    3) merge：target_entity_id 必须是证据最全、最完整的实体，其余碎片进 source_entity_ids；\
    4) drop：只删除确为噪音的实体，且只允许 source_kind='ai' 的 AI 归属实体；\
       bilibili_tag / category / creator 等平台事实实体绝不能删除，也不应被并入其它实体；\
    5) 每个动作的 rationale 必须给出依据（非空、≤200 字）；\
    6) 证据不足/毫不相识：什么都不提交（提交空计划）。\
    纪律：你只决定「谁归并、谁删除」，ID、存在性、时间等由程序校验。普通文本不是有效输出，\
    必须调用 submit_entity_reconcile 提交你的计划。"
        .to_string()
}

fn reconcile_user_prompt(total: usize) -> String {
    format!("当前长期实体注册表共 {total} 个实体。请读完整表分页，判断碎片化并提交归并计划。")
}

/// 跑一轮实体归并。Err = 协议/网络/拒绝/部分失败——调用方响铃并把归并留空。
pub async fn run_entity_reconcile(
    runtime: &AgentRuntime,
    config: &Config,
    store: &Store,
) -> Result<EntityReconcileReport, AgentRuntimeError> {
    let mut ctx = ReconcileContext {
        store: Store::open(&config.output_dir.join("graph").join("perception.sqlite3"))
            .map_err(|err| AgentRuntimeError::Protocol(format!("reconcile 打开图库失败：{err}")))?,
        submission: None,
        slot: SubmissionSlot::default(),
    };
    let total = ctx
        .store
        .count_scalar("SELECT COUNT(*) FROM entities", &[])
        .unwrap_or(0);
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
    let prompt = reconcile_user_prompt(total as usize);
    let outcome = run_toolcall_agent::<ReconcileContext, EntityReconcileDraft>(
        runtime,
        &mut spec,
        AttemptPlan {
            label: "entity-reconcile",
            prompt: &prompt,
            max_turns: config.ai.agent.max_turns as usize,
            retries: config.ai.agent.run_retries.max(0) as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
            token_budget: None,
        },
        &mut ctx,
        &mut trace,
    )
    .await?;
    let draft = outcome.submission;

    // 空计划不记账：无动作的 maintenance run 会照常进 run_pair_delta 的
    // 「相邻 complete」对照窗，把「vs 上轮感知」稀释成无变化（P0-4 同类坑
    // 已用 kind 排除 recap-refresh 钉过一次）——没消费者就不出生。
    if draft.merges.is_empty() && draft.drops.is_empty() {
        return Ok(EntityReconcileReport {
            planned: draft,
            merged_ok: 0,
            dropped_ok: 0,
            failed: Vec::new(),
        });
    }

    // 程序侧事实层：一次外层 maintenance run 记账，逐笔执行裁决。
    let detail_json = serde_json::to_string(&draft).ok();
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
    Ok(EntityReconcileReport {
        planned: draft,
        merged_ok,
        dropped_ok,
        failed,
    })
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
