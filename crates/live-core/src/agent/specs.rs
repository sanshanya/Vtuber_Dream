//! M3-D 终局工具厂 + Agent 装配（自 tools.rs 整平移，速度拆分见 review 工程 m4）。
//!
//! 消费者：pipeline（M4）与 tests/agent_golden.rs。这里的两个 spec 装配器带上
//! Python pipeline.py 的 name/instructions/tools 顺序 parity。

use std::collections::{BTreeSet, HashSet};

use serde_json::json;

use crate::graph::query;
use crate::models::{AudienceSituationSubmission, ViewerPerceptionSubmission};

use super::prompts::{audience_instructions, viewer_instructions};
use super::runtime::{AgentSpec, AgentTool, TerminalOutcome, make_terminal_tool};
use super::tools::{
    AudienceAgentCtx, ViewerAgentCtx, audience_investigation_tools, known_search_result_ids,
    py_or_empty, viewer_investigation_tools,
};
use super::validators::{validate_audience_submission, validate_viewer_submission};

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
            let search_ids: HashSet<String> = known_search_result_ids(&ctx.research);
            // R4：实体存在性查询是基础设施面——预探测，DB 故障走 Fatal
            // （不再 unwrap_or(false) 白标为"entity 不存在"的校验拒收）。
            let mut probe_ids = BTreeSet::new();
            for entity in &submission.entities {
                if let Some(existing) = &entity.existing_entity_id
                    && !existing.is_empty()
                {
                    probe_ids.insert(existing.clone());
                }
                probe_ids.extend(entity.parent_entity_refs.iter().cloned());
            }
            probe_ids.extend(submission.mentions.iter().map(|m| m.entity_ref.clone()));
            probe_ids.extend(
                submission
                    .relations
                    .iter()
                    .flat_map(|r| [r.subject_ref.clone(), r.object_ref.clone()]),
            );
            probe_ids.extend(
                submission
                    .interest_states
                    .iter()
                    .map(|s| s.entity_ref.clone()),
            );
            let mut exists_map = std::collections::HashMap::new();
            for candidate_id in probe_ids {
                match ctx.store.entity_exists(&candidate_id) {
                    Ok(found) => {
                        exists_map.insert(candidate_id, found);
                    }
                    Err(err) => {
                        return TerminalOutcome::Fatal(format!("entity lookup failed: {err}"));
                    }
                }
            }
            let entity_exists =
                |candidate_id: &str| exists_map.get(candidate_id).copied().unwrap_or(false);
            let errors = validate_viewer_submission(
                submission,
                &viewer_id,
                &ctx.episodes,
                &entity_exists,
                &search_ids,
            );
            if errors.is_empty() {
                TerminalOutcome::Accept(json!({
                    "viewer_id": submission.viewer_id,
                    "mentions": submission.mentions.len(),
                    "entities": submission.entities.len(),
                    "relations": submission.relations.len(),
                    "interest_states": submission.interest_states.len(),
                }))
            } else {
                TerminalOutcome::Reject(errors)
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
            // R4：references 失败 = 基础设施故障 → Fatal（不白标为模型可修正的校验拒收）。
            let references = match query::references(&ctx.store, &entity_refs, &[], &mention_refs) {
                Ok(references) => references,
                Err(err) => {
                    return TerminalOutcome::Fatal(format!("graph references failed: {err}"));
                }
            };
            let to_hashset = |key: &str| -> HashSet<String> {
                references
                    .get(key)
                    .map(|set| set.iter().cloned().collect())
                    .unwrap_or_default()
            };
            let viewer_ids: HashSet<String> = ctx.viewer_analyses.keys().cloned().collect();
            let search_ids: HashSet<String> = known_search_result_ids(&ctx.research);
            let errors = validate_audience_submission(
                submission,
                &viewer_ids,
                &to_hashset("entities"),
                &to_hashset("mentions"),
                &search_ids,
            );
            if errors.is_empty() {
                TerminalOutcome::Accept(json!({
                    "interest_items": submission.interest_graph.len(),
                    "communities": submission.communities.len(),
                    "situations": submission.situations.len(),
                    "content_opportunities": submission.content_opportunities.len(),
                }))
            } else {
                TerminalOutcome::Reject(errors)
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
