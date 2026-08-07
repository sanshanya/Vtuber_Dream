//! M3-D 终局工具厂 + Agent 装配（自 tools.rs 整平移，速度拆分见 review 工程 m4）。
//!
//! 消费者：pipeline（M4）与 tests/agent_golden.rs。这里的两个 spec 装配器带上
//! Python pipeline.py 的 name/instructions/tools 顺序 parity。G2-A1：viewer 装配
//! 在冻结四件之外有唯一白名单增量 verify_videos（agent_golden 白名单机制对照）；
//! audience 装配无增量（4+1 冻结）。

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

/// 工具规格版本串：工具 name/description/参数 schema 变更时同步递增，
/// 与 prompts::PROMPTS_VERSION 一同写入 trace 的 run_start（协议红线：变更可审计）。
/// G2-A1：viewer 装配入 verify_videos（白名单增量，见 tools.rs 装配注与 golden
/// 注记 fixture agent_tool_list_note.json）→ .v1 → .v2。
/// audience 终局 schema 增 front_brief（制片人简报）→ 2026-08-05.v1。
pub const TOOL_SPECS_VERSION: &str = "2026-08-05.v1";

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
            // 实体存在性查询是基础设施面——预探测，DB 故障走 Fatal
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
            // references 失败 = 基础设施故障 → Fatal（不白标为模型可修正的校验拒收）。
            let references = match query::references(&ctx.store, &entity_refs, &[], &mention_refs) {
                Ok(references) => references,
                Err(err) => {
                    return TerminalOutcome::Fatal(format!("graph references failed: {err}"));
                }
            };
            fn to_hashset_from(
                references: &std::collections::HashMap<String, std::collections::BTreeSet<String>>,
                key: &str,
            ) -> HashSet<String> {
                references
                    .get(key)
                    .map(|set| set.iter().cloned().collect())
                    .unwrap_or_default()
            }
            let to_hashset = |key: &str| -> HashSet<String> { to_hashset_from(&references, key) };
            let viewer_ids: HashSet<String> = ctx.viewer_analyses.keys().cloned().collect();
            let search_ids: HashSet<String> = known_search_result_ids(&ctx.research);
            let mut errors = validate_audience_submission(
                submission,
                &viewer_ids,
                &to_hashset("entities"),
                &to_hashset("mentions"),
                &search_ids,
            );
            // 简报句句带出处——episode_refs 过 episodes 桶存在性闭包
            // （references 通道同型；图谱 append-only，restore 面免复验）。
            if !submission.front_brief.sentences.is_empty() {
                let episode_refs: Vec<String> = submission
                    .front_brief
                    .sentences
                    .iter()
                    .flat_map(|sentence| sentence.episode_refs.iter().cloned())
                    .collect();
                match query::references(&ctx.store, &[], &episode_refs, &[]) {
                    Ok(brief_refs) => {
                        let known = to_hashset_from(&brief_refs, "episodes");
                        for (index, sentence) in submission.front_brief.sentences.iter().enumerate()
                        {
                            let unknown: Vec<String> = sentence
                                .episode_refs
                                .iter()
                                .filter(|episode_id| {
                                    !episode_id.is_empty() && !known.contains(*episode_id)
                                })
                                .cloned()
                                .collect();
                            if !unknown.is_empty() {
                                errors.push(format!(
                                    "front_brief.sentences[{index}] references unknown episodes: {}",
                                    unknown
                                        .iter()
                                        .map(|value| format!(r#""{value}""#))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ));
                            }
                        }
                    }
                    Err(err) => {
                        return TerminalOutcome::Fatal(format!("graph references failed: {err}"));
                    }
                }
            }
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
