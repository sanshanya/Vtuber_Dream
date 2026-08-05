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

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::episodes::{Episode, validate_span};
use crate::models::{AudienceSituationSubmission, Lead, ViewerPerceptionSubmission};

/// 修复 8：摘要去空白后低于此字符数即视为"不是一份真分析"（S0 实测短摘要曾被接受）。
pub const SUMMARY_MIN_CHARS: usize = 16;
/// 摘要上限（Python ai_models Text100000 = max_length=100_000 的 decode 层硬拒，
/// 落进业务校验层；超限时 Python 在 pydantic 阶段就拒，此处文案为本仓自定）。
pub const SUMMARY_MAX_CHARS: usize = 100_000;
/// 修复 8：占位文本集合（去空白后完全相等即拒；S0 实测 "测试"/"summary" 曾被接受）。
pub const PLACEHOLDER_SUMMARIES: [&str; 4] = ["测试", "test", "summary", "占位"];
/// 修复 8：字符串栏目"有实质内容"判定 = 至少一条去空白后字符数 ≥ 2（S0 实测 ["a"] 曾被接受）。
pub const SECTION_SUBSTANTIVE_MIN_CHARS: usize = 2;
/// leads.type 白名单（kickoff §M4.x 薄切提前到 M3：schema + 校验随终局协议进入）。
/// MXA-7/12：唯一真源在 leads.rs（annex 面与校验面共一张清单）。
pub const LEAD_TYPE_WHITELIST: [&str; 4] = crate::leads::LEAD_TYPES;

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
    items.iter().any(|item| {
        let stripped = item.trim();
        char_len(stripped) >= SECTION_SUBSTANTIVE_MIN_CHARS
            && !PLACEHOLDER_SUMMARIES.contains(&stripped)
    })
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
    if char_len(stripped) > SUMMARY_MAX_CHARS {
        errors.push(format!("{field} exceeds max length {SUMMARY_MAX_CHARS}"));
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
                // 轮2-R1-A⑦：真源 LEAD_TYPES 若长第 5 型，白名单放行但本臂落
                // unreachable!() 会在生产 panic——校验层只许拒收不许崩溃。
                other => errors.push(format!(
                    "{label} locator has no validation rule for type {other}（新类型须先补 locator 规则）"
                )),
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
    episodes: &BTreeMap<String, Episode>,
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
            // Python `and entity.existing_entity_id`（truthiness）：Some("") 不触发（评审3-M1）
            "NEW_ENTITY"
                if entity
                    .existing_entity_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty()) =>
            {
                errors.push(format!(
                    "entity {} cannot set existing_entity_id with NEW_ENTITY",
                    entity.local_id
                ))
            }
            "UNCERTAIN"
                if entity
                    .existing_entity_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty()) =>
            {
                errors.push(format!(
                    "entity {} cannot set existing_entity_id with UNCERTAIN",
                    entity.local_id
                ))
            }
            // SAME_AS 已被上方无守卫臂吃掉；此处只需接住守卫未命中的另两个合法值。
            "NEW_ENTITY" | "UNCERTAIN" => {}
            // 2026-08-05 生产事故（v4-flash 真实提交 EXISTING）：未知取值不得穿透到
            // 图写入层（store/entities.rs 的 unknown decision）——拒收文案必须带回
            // 原值与合法取值表，让模型在同一个 Agent Loop 内自纠。
            other => errors.push(format!(
                "entity {} has unknown resolution: {other:?} (allowed: SAME_AS, NEW_ENTITY, UNCERTAIN)",
                entity.local_id
            )),
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

    // 2026-08-05 生产事故（观众 77044362 graph_failed）：build.rs 要求同一 target
    // 只有一条 INTERESTED_IN；两条 state 指向重复时必须**在工具层**拒收并指名冲突面。
    // 去重分两级：①entity_ref 原样重复；②SAME_AS 跨解析撞车——本地实体映射到某
    // 现有实体，另一 state 又直引同一个现有实体 id。
    let same_as_targets: HashMap<String, String> = submission
        .entities
        .iter()
        .filter(|entity| {
            entity.resolution == "SAME_AS"
                && entity
                    .existing_entity_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty())
        })
        .map(|entity| {
            (
                entity.local_id.clone(),
                entity.existing_entity_id.clone().unwrap_or_default(),
            )
        })
        .collect();
    let mut seen_targets: Vec<String> = Vec::new();
    for state in &submission.interest_states {
        let stripped = state.entity_ref.strip_prefix("entity:").unwrap_or("");
        let target_key = same_as_targets
            .get(stripped)
            .cloned()
            .unwrap_or_else(|| state.entity_ref.clone());
        if seen_targets.contains(&target_key) {
            errors.push(format!(
                "interest states have duplicate target: {} (resolve to the same entity; merge the states into one)",
                state.entity_ref
            ));
        } else {
            seen_targets.push(target_key);
        }
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
    // 轮2-R1-A⑦：内容偏好/近期变化/深挖目标是真实承载观众观点的栏——只有它们
    // 单独的合法提交不得被判空（守卫口的空判定与 AI 实质判定必须同一边界）。
    if submission.entities.is_empty()
        && submission.interest_states.is_empty()
        && !has_substantive(&submission.hypotheses)
        && !has_substantive(&submission.cautions)
        && !has_substantive(&submission.content_preferences)
        && !has_substantive(&submission.recent_changes)
        && !has_substantive(&submission.enrichment_targets)
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

// ---------------------------------------------------------------------------
// Z5/C1 front_brief 结构校验（P0-5）
// ---------------------------------------------------------------------------

/// 简报句数上限（终裁：弹性、不收刚性 3+3+1，但 payload 必须有界）。
pub const BRIEF_SENTENCE_CAP: usize = 12;
/// 单句结论长度上限（字符）。
pub const BRIEF_TEXT_MAX_CHARS: usize = 500;

/// ISO 日期/时间字符串可解析（YYYY-MM-DD 或 RFC3339）。
fn parse_iso_moment(text: &str) -> Option<chrono::NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|dt| dt.naive_utc())
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .map(|date| date.and_hms_opt(0, 0, 0).expect("午夜恒合法"))
                .ok()
        })
}

/// Z5/C1 结构面：句数上限 / 非空结论 / 句句带出处（≥1 episode_ref）/ 覆盖时段
/// 可解析且 from<=to。episode 存在性闭包不在此——图谱 append-only，存在性只在
/// specs 终局闭包一次性核验（references 通道），restore 复核天然保持。
pub fn validate_front_brief(brief: &crate::models::FrontBrief) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if brief.sentences.len() > BRIEF_SENTENCE_CAP {
        errors.push(format!(
            "front_brief.sentences exceeds cap {BRIEF_SENTENCE_CAP}"
        ));
    }
    for (index, sentence) in brief.sentences.iter().enumerate() {
        let label = format!("front_brief.sentences[{index}]");
        if sentence.text.trim().is_empty() {
            errors.push(format!("{label} text cannot be empty"));
        } else if sentence.text.chars().count() > BRIEF_TEXT_MAX_CHARS {
            errors.push(format!("{label} text exceeds {BRIEF_TEXT_MAX_CHARS} chars"));
        }
        if sentence.episode_refs.is_empty() {
            errors.push(format!("{label} must reference at least one episode"));
        }
        if let Some([from, to]) = &sentence.coverage_time_range {
            match (parse_iso_moment(from), parse_iso_moment(to)) {
                (Some(from_m), Some(to_m)) if from_m <= to_m => {}
                (Some(_), Some(_)) => {
                    errors.push(format!("{label} coverage_time_range from must be <= to"))
                }
                _ => errors.push(format!(
                    "{label} coverage_time_range must be ISO date/datetime strings"
                )),
            }
        }
    }
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
    // Z5/C1：front_brief 结构先于栏目闭包（简报是首屏面，错误要早现）。
    let mut errors: Vec<String> = validate_front_brief(&submission.front_brief);

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
