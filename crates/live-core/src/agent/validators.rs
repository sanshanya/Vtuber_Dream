//! 终局校验台：Python `validators.py` 平移 + 设计文档 §9.1 修复 1/6/8 + leads（M4.x 提前出生）。
//!
//! 修复 1：ContentOpportunity 必须引真实 mention_id（关闭 Python
//! `required=not bool(search_result_ids)` 逃逸口）。
//! 修复 6：a) origin↔kind 绑定——平台结构化字段（`platform_*` kind）上的 mention 必须
//! `origin="platform"`（单向；text 字段上的 tag 重现 mention 用 platform origin 合法，
//! 与 seeds.rs 的 platform_tag_in_text 发射一致）；b) audience 空串 entity_id 拒（关闭
//! Python `if item and ...` 守卫）+ interest_graph entity_id 非空拒；c) search_result_ids
//! 按运行隔离——调用方只准传本运行 `known_search_result_ids(research)`，校验器负例钉死。
//! 修复 8：占位符/空提交拒绝；阈值 Python 无既有值，为本仓自定（常量命名 + 负例测试）。

use std::collections::{HashMap, HashSet};

use crate::episodes::{Episode, validate_span};
use crate::models::{AudienceSituationSubmission, Lead, ViewerPerceptionSubmission};

/// 修复 8：摘要去空白后低于此字符数即视为"不是一份真分析"（S0 实测短摘要曾被接受）。
pub const SUMMARY_MIN_CHARS: usize = 16;
/// 修复 8：占位文本集合（去空白后完全相等即拒；S0 实测 "测试"/"summary" 曾被接受）。
pub const PLACEHOLDER_SUMMARIES: [&str; 4] = ["测试", "test", "summary", "占位"];
/// 修复 8：字符串栏目"有实质内容"判定 = 至少一条去空白后字符数 ≥ 2（S0 实测 ["a"] 曾被接受）。
pub const SECTION_SUBSTANTIVE_MIN_CHARS: usize = 2;
/// leads.type 白名单（kickoff §M4.x 薄切提前到 M3：schema + 校验随终局协议进入）。
pub const LEAD_TYPE_WHITELIST: [&str; 4] = ["search", "creator", "video", "room"];

const ERROR_CAP: usize = 100;

/// Python f-string 的 list repr 形态（`['a', 'b']`），保持错误文案与 Python 一致。
fn py_list(items: &[String]) -> String {
    let inner = items
        .iter()
        .map(|item| format!("'{item}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

fn unknown_items(values: &[String], known: &HashSet<String>) -> Vec<String> {
    values
        .iter()
        .filter(|item| !known.contains(item.as_str()))
        .cloned()
        .collect()
}

fn unknown_in_map(
    values: &[String],
    known: &HashMap<String, crate::models::MentionSpan>,
) -> Vec<String> {
    values
        .iter()
        .filter(|item| !known.contains_key(item.as_str()))
        .cloned()
        .collect()
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// 字符串列表是否存在"有实质内容"的一条（去空白后字符数 ≥ SECTION_SUBSTANTIVE_MIN_CHARS）。
fn has_substantive(items: &[String]) -> bool {
    items
        .iter()
        .any(|item| char_len(item.trim()) >= SECTION_SUBSTANTIVE_MIN_CHARS)
}

fn check_summary(field: &str, summary: &str, errors: &mut Vec<String>) {
    let stripped = summary.trim();
    if PLACEHOLDER_SUMMARIES.contains(&stripped) {
        errors.push(format!("{field} is placeholder text"));
        return;
    }
    if char_len(stripped) < SUMMARY_MIN_CHARS {
        errors.push(format!("{field} is too short to be a real analysis"));
    }
}

/// leads 校验：type 白名单 + locator 形态 + 必填一句 + evidence 闭包 + UNCERTAIN 不得驱动。
/// closure：viewer 侧 = 本提交 mention_ids ∪ entity local_ids；audience 侧 = 传入的两集合。
fn validate_leads(
    leads: &[Lead],
    closure: &HashSet<String>,
    uncertain_local_ids: &HashSet<String>,
    errors: &mut Vec<String>,
) {
    for (index, lead) in leads.iter().enumerate() {
        let label = format!("leads[{index}]");
        if !LEAD_TYPE_WHITELIST.contains(&lead.lead_type.as_str()) {
            errors.push(format!(
                "{label} type must be one of {}",
                py_list(&LEAD_TYPE_WHITELIST.map(String::from))
            ));
        } else {
            match lead.lead_type.as_str() {
                "search" => {
                    if lead.locator.trim().is_empty() {
                        errors.push(format!("{label} locator cannot be empty"));
                    }
                }
                "creator" | "room" => {
                    if lead.locator.is_empty()
                        || !lead.locator.chars().all(|ch| ch.is_ascii_digit())
                    {
                        errors.push(format!(
                            "{label} locator must be numeric for type {}",
                            lead.lead_type
                        ));
                    }
                }
                "video" => {
                    if !lead.locator.starts_with("BV") {
                        errors.push(format!("{label} locator must be a BV id for type video"));
                    }
                }
                _ => unreachable!(),
            }
        }
        for (field, value) in [
            ("motivation", &lead.motivation),
            ("expected_signal", &lead.expected_signal),
            ("priority", &lead.priority),
        ] {
            if value.trim().is_empty() {
                errors.push(format!("{label} {field} cannot be empty"));
            }
        }
        let unknown = unknown_items(&lead.evidence_ids, closure);
        if !unknown.is_empty() {
            errors.push(format!(
                "{label} references unknown evidence: {}",
                py_list(&unknown)
            ));
        }
        let driven_by_uncertain: Vec<String> = lead
            .evidence_ids
            .iter()
            .filter(|item| uncertain_local_ids.contains(item.as_str()))
            .cloned()
            .collect();
        for local_id in driven_by_uncertain {
            errors.push(format!(
                "{label} must not be driven by UNCERTAIN entity: {local_id}"
            ));
        }
    }
}

/// 观众级终局校验（Python `validate_viewer_submission` 平移 + 修复 6a/8 + leads）。
/// `search_result_ids` 必须是本运行 ResearchService 的快照注册表（修复 6c 接线纪律）。
pub fn validate_viewer_submission(
    submission: &ViewerPerceptionSubmission,
    viewer_id: &str,
    episodes: &HashMap<String, Episode>,
    entity_exists: &dyn Fn(&str) -> bool,
    search_result_ids: &HashSet<String>,
) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if submission.viewer_id != viewer_id {
        errors.push(format!("viewer_id must be {viewer_id}"));
    }

    let mut mention_map: HashMap<String, crate::models::MentionSpan> = HashMap::new();
    for mention in &submission.mentions {
        if mention_map.contains_key(&mention.mention_id) {
            errors.push(format!("duplicate mention_id: {}", mention.mention_id));
            continue;
        }
        mention_map.insert(mention.mention_id.clone(), mention.clone());
        let episode = match episodes.get(&mention.episode_id) {
            Some(episode) => episode,
            None => {
                errors.push(format!("unknown episode_id: {}", mention.episode_id));
                continue;
            }
        };
        if let Some(span_error) = validate_span(
            episode,
            &mention.field_path,
            &mention.text,
            mention.start,
            mention.end,
        ) {
            errors.push(span_error);
        }
        // 修复 6a：平台结构化字段上的 mention 是平台事实，必须标 origin="platform"。
        if let Some(field) = episode
            .fields
            .iter()
            .find(|field| field.path == mention.field_path)
            && field.kind.starts_with("platform_")
            && mention.origin != "platform"
        {
            errors.push(format!(
                "mention {} has origin \"{}\" on platform field {}",
                mention.mention_id, mention.origin, mention.field_path
            ));
        }
    }

    let mut entity_map: HashMap<String, crate::models::EntityProposal> = HashMap::new();
    let mut grounded_new: HashMap<(String, Vec<String>), String> = HashMap::new();
    for entity in &submission.entities {
        if entity_map.contains_key(&entity.local_id) {
            errors.push(format!("duplicate entity local_id: {}", entity.local_id));
            continue;
        }
        entity_map.insert(entity.local_id.clone(), entity.clone());
        let unknown = unknown_in_map(&entity.evidence_mention_ids, &mention_map);
        if !unknown.is_empty() {
            errors.push(format!(
                "entity {} references unknown mentions: {}",
                entity.local_id,
                py_list(&unknown)
            ));
        }
        if entity.evidence_mention_ids.is_empty() {
            errors.push(format!(
                "entity {} must reference at least one grounded mention",
                entity.local_id
            ));
        }
        match entity.resolution.as_str() {
            "SAME_AS" => match &entity.existing_entity_id {
                // Python `not entity.existing_entity_id`：None 与空串同拒。
                None => errors.push(format!(
                    "entity {} uses SAME_AS without existing_entity_id",
                    entity.local_id
                )),
                Some(existing) if existing.is_empty() => errors.push(format!(
                    "entity {} uses SAME_AS without existing_entity_id",
                    entity.local_id
                )),
                Some(existing) => {
                    if !entity_exists(existing) {
                        errors.push(format!(
                            "entity {} points to unknown existing entity",
                            entity.local_id
                        ));
                    }
                }
            },
            "NEW_ENTITY" if entity.existing_entity_id.is_some() => errors.push(format!(
                "entity {} cannot set existing_entity_id with NEW_ENTITY",
                entity.local_id
            )),
            "UNCERTAIN" if entity.existing_entity_id.is_some() => errors.push(format!(
                "entity {} cannot set existing_entity_id with UNCERTAIN",
                entity.local_id
            )),
            _ => {}
        }
        if entity.resolution == "NEW_ENTITY" && unknown.is_empty() {
            let mut identity_ids = entity.evidence_mention_ids.clone();
            identity_ids.sort();
            identity_ids.dedup();
            let identity = (entity.entity_type.clone(), identity_ids);
            match grounded_new.get(&identity) {
                None => {
                    grounded_new.insert(identity, entity.local_id.clone());
                }
                Some(previous) => errors.push(format!(
                    "entities {} and {} have duplicate grounded NEW_ENTITY identity",
                    previous, entity.local_id
                )),
            }
        }
    }

    for mention in &submission.mentions {
        let entity_ref = &mention.entity_ref;
        let local_id = entity_ref.strip_prefix("entity:").unwrap_or(entity_ref);
        if !(entity_map.contains_key(local_id)
            || entity_ref.starts_with("entity:") && entity_exists(entity_ref))
        {
            errors.push(format!(
                "mention {} has unknown entity_ref: {}",
                mention.mention_id, entity_ref
            ));
        }
    }

    let mut valid_refs: HashSet<String> = HashSet::from(["viewer:self".to_string()]);
    for episode_id in episodes.keys() {
        valid_refs.insert(format!(
            "episode:{}",
            episode_id.strip_prefix("episode:").unwrap_or(episode_id)
        ));
    }
    valid_refs.extend(episodes.keys().cloned());
    valid_refs.extend(entity_map.keys().cloned());
    valid_refs.extend(
        entity_map
            .keys()
            .map(|local_id| format!("entity:{local_id}")),
    );

    let local_entity = |entity_ref: &str| -> Option<&crate::models::EntityProposal> {
        entity_map.get(entity_ref.strip_prefix("entity:").unwrap_or(entity_ref))
    };
    let known_ref = |entity_ref: &str, formal: bool| -> bool {
        if let Some(entity) = local_entity(entity_ref) {
            return !formal || entity.resolution != "UNCERTAIN";
        }
        valid_refs.contains(entity_ref)
            || (entity_ref.starts_with("entity:") && entity_exists(entity_ref))
    };
    let uncertain_suffix = |entity_ref: &str| -> &str {
        if local_entity(entity_ref).is_some() {
            " (UNCERTAIN entities are not formal)"
        } else {
            ""
        }
    };

    for entity in &submission.entities {
        for parent_ref in &entity.parent_entity_refs {
            if !known_ref(parent_ref, true) {
                errors.push(format!(
                    "entity {} has unknown parent_ref: {}",
                    entity.local_id, parent_ref
                ));
            }
        }
    }

    for relation in &submission.relations {
        for (role, entity_ref) in [
            ("subject_ref", &relation.subject_ref),
            ("object_ref", &relation.object_ref),
        ] {
            if !known_ref(entity_ref, true) {
                errors.push(format!(
                    "relation has unknown {role}: {}{}",
                    entity_ref,
                    uncertain_suffix(entity_ref)
                ));
            }
        }
        let unknown = unknown_in_map(&relation.evidence_mention_ids, &mention_map);
        if !unknown.is_empty() {
            errors.push(format!(
                "relation references unknown mentions: {}",
                py_list(&unknown)
            ));
        }
        if relation.evidence_mention_ids.is_empty() {
            errors.push(format!(
                "relation {} must reference grounded mentions",
                relation.predicate
            ));
        }
    }

    for state in &submission.interest_states {
        if !known_ref(&state.entity_ref, true) {
            errors.push(format!(
                "interest state has unknown entity_ref: {}{}",
                state.entity_ref,
                uncertain_suffix(&state.entity_ref)
            ));
        }
        let unknown = unknown_in_map(&state.evidence_mention_ids, &mention_map);
        if !unknown.is_empty() {
            errors.push(format!(
                "interest state references unknown mentions: {}",
                py_list(&unknown)
            ));
        }
        if state.evidence_mention_ids.is_empty() {
            errors.push(format!(
                "interest state {} must reference grounded mentions",
                state.entity_ref
            ));
        }
    }

    for (group_name, actions) in [
        ("conversation_openers", &submission.conversation_openers),
        ("content_ideas", &submission.content_ideas),
    ] {
        for action in actions {
            if action.title.trim().is_empty() {
                errors.push(format!("{group_name} action title cannot be empty"));
            }
            let unknown = unknown_in_map(&action.evidence_mention_ids, &mention_map);
            if !unknown.is_empty() {
                errors.push(format!(
                    "{group_name} action references unknown mentions: {}",
                    py_list(&unknown)
                ));
            }
            let unknown_search = unknown_items(&action.search_result_ids, search_result_ids);
            if !unknown_search.is_empty() {
                errors.push(format!(
                    "{group_name} action references unknown search results: {}",
                    py_list(&unknown_search)
                ));
            }
        }
    }

    // 修复 8：占位/空提交拒绝。
    check_summary("profile_summary", &submission.profile_summary, &mut errors);
    if submission.entities.is_empty()
        && submission.interest_states.is_empty()
        && !has_substantive(&submission.hypotheses)
        && !has_substantive(&submission.cautions)
    {
        errors.push(
            "viewer submission has empty entities and interest_states; provide hypotheses or cautions"
                .to_string(),
        );
    }

    // leads（M4.x 提前）：evidence 闭包 = 本提交 mention_ids ∪ entity local_ids；
    // UNCERTAIN 实体 local_id 不得驱动线索。
    let closure: HashSet<String> = mention_map
        .keys()
        .chain(entity_map.keys())
        .cloned()
        .collect();
    let uncertain_local_ids: HashSet<String> = entity_map
        .values()
        .filter(|entity| entity.resolution == "UNCERTAIN")
        .map(|entity| entity.local_id.clone())
        .collect();
    validate_leads(
        &submission.leads,
        &closure,
        &uncertain_local_ids,
        &mut errors,
    );

    errors.truncate(ERROR_CAP);
    errors
}

/// 整体态势终局校验（Python `validate_audience_submission` 平移 + 修复 1/6b/8 + leads）。
/// 四个 id 集合均由调用方按本运行组装（viewer 提交并集 / 图实体 / 图 mention / 搜索注册表）。
pub fn validate_audience_submission(
    submission: &AudienceSituationSubmission,
    viewer_ids: &HashSet<String>,
    entity_ids: &HashSet<String>,
    mention_ids: &HashSet<String>,
    search_result_ids: &HashSet<String>,
) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();

    let check_viewers = |values: &[String], label: String, errors: &mut Vec<String>| {
        let unknown = unknown_items(values, viewer_ids);
        if !unknown.is_empty() {
            errors.push(format!(
                "{label} references unknown viewers: {}",
                py_list(&unknown)
            ));
        }
    };
    // 修复 6b：不清掉空串（Python `if item and ...` 守卫），空 entity_id 直接进 unknown 清单被拒。
    let check_entities = |values: &[String], label: String, errors: &mut Vec<String>| {
        let unknown = unknown_items(values, entity_ids);
        if !unknown.is_empty() {
            errors.push(format!(
                "{label} references unknown entities: {}",
                py_list(&unknown)
            ));
        }
    };
    let check_mentions =
        |values: &[String], label: String, required: bool, errors: &mut Vec<String>| {
            let unknown = unknown_items(values, mention_ids);
            if !unknown.is_empty() {
                errors.push(format!(
                    "{label} references unknown mentions: {}",
                    py_list(&unknown)
                ));
            }
            if required && values.is_empty() {
                errors.push(format!("{label} must reference grounded mentions"));
            }
        };

    for (index, item) in submission.interest_graph.iter().enumerate() {
        let label = format!("interest_graph[{index}]");
        check_viewers(&item.viewer_ids, label.clone(), &mut errors);
        // 修复 6b：interest_graph 以实体为锚，entity_id 空串显式拒（不走 unknown-entities 文案）。
        if item.entity_id.trim().is_empty() {
            errors.push(format!("{label} entity_id cannot be empty"));
        } else {
            check_entities(
                std::slice::from_ref(&item.entity_id),
                label.clone(),
                &mut errors,
            );
        }
        check_mentions(&item.evidence_mention_ids, label.clone(), true, &mut errors);
    }
    for (index, item) in submission.communities.iter().enumerate() {
        let label = format!("communities[{index}]");
        check_viewers(&item.viewer_ids, label.clone(), &mut errors);
        check_entities(&item.entity_ids, label.clone(), &mut errors);
        check_mentions(&item.evidence_mention_ids, label.clone(), true, &mut errors);
    }
    for (index, item) in submission.situations.iter().enumerate() {
        let label = format!("situations[{index}]");
        check_viewers(&item.viewer_ids, label.clone(), &mut errors);
        check_entities(&item.entity_ids, label.clone(), &mut errors);
        check_mentions(&item.evidence_mention_ids, label.clone(), true, &mut errors);
    }
    for (index, item) in submission.content_opportunities.iter().enumerate() {
        let label = format!("content_opportunities[{index}]");
        check_viewers(&item.audience_ids, label.clone(), &mut errors);
        if !item.entity_id.is_empty() {
            check_entities(
                std::slice::from_ref(&item.entity_id),
                label.clone(),
                &mut errors,
            );
        }
        // 修复 1：mentions 恒为必需（关闭 Python `required=not bool(search_result_ids)` 逃逸口）。
        check_mentions(&item.evidence_mention_ids, label.clone(), true, &mut errors);
        let unknown_search = unknown_items(&item.search_result_ids, search_result_ids);
        if !unknown_search.is_empty() {
            errors.push(format!(
                "{label} references unknown search results: {}",
                py_list(&unknown_search)
            ));
        }
    }
    for (index, item) in submission.individual_highlights.iter().enumerate() {
        let label = format!("individual_highlights[{index}]");
        check_viewers(
            std::slice::from_ref(&item.viewer_id),
            label.clone(),
            &mut errors,
        );
        check_mentions(&item.evidence_mention_ids, label.clone(), true, &mut errors);
    }
    for (index, item) in submission.content_calendar.iter().enumerate() {
        let label = format!("content_calendar[{index}]");
        check_viewers(&item.target_viewers, label.clone(), &mut errors);
    }

    // 修复 8：占位摘要 + 至少一个栏目非空。
    check_summary(
        "executive_summary",
        &submission.executive_summary,
        &mut errors,
    );
    let any_section = !submission.interest_graph.is_empty()
        || !submission.communities.is_empty()
        || !submission.situations.is_empty()
        || !submission.content_opportunities.is_empty()
        || !submission.individual_highlights.is_empty()
        || !submission.content_calendar.is_empty()
        || has_substantive(&submission.audience_structure)
        || has_substantive(&submission.data_gaps)
        || has_substantive(&submission.safety_notes);
    if !any_section {
        errors.push("at least one audience section must be non-empty".to_string());
    }

    let mut closure: HashSet<String> = mention_ids.clone();
    closure.extend(entity_ids.iter().cloned());
    validate_leads(&submission.leads, &closure, &HashSet::new(), &mut errors);

    errors.truncate(ERROR_CAP);
    errors
}
