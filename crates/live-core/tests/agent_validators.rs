//! M3-C 终局校验台负例矩阵：Python test_*_validator_semantics.py family 平移
//! +§9.1 修复 1/6/8 回归 + leads 校验。

use std::collections::{BTreeMap, HashSet};

use live_core::agent::validators::{validate_audience_submission, validate_viewer_submission};
use live_core::episodes::{Episode, EpisodeField};
use live_core::models::{
    AudienceSituationSubmission, BriefSentence, ContentOpportunity, EntityProposal,
    GraphInterestItem, InterestStateProposal, Lead, MentionSpan, RelationProposal, ViewerAction,
    ViewerPerceptionSubmission,
};
use serde_json::json;

const VIEWER: &str = "v1";
const EP1: &str = "episode:v1:ep1";
const TITLE_TEXT: &str = "玩塞尔达旷野之息真上头";

fn episode() -> Episode {
    Episode {
        episode_id: EP1.to_string(),
        viewer_id: VIEWER.to_string(),
        source: "video".to_string(),
        event_type: "video".to_string(),
        observed_at: "2026-08-04T00:00:00+00:00".to_string(),
        published_at: "2026-08-01T00:00:00+00:00".to_string(),
        title: TITLE_TEXT.to_string(),
        url: String::new(),
        bvid: String::new(),
        fields: vec![
            EpisodeField {
                path: "title".to_string(),
                text: TITLE_TEXT.to_string(),
                kind: "text".to_string(),
            },
            EpisodeField {
                path: "tags[0]".to_string(),
                text: "塞尔达传说".to_string(),
                kind: "platform_tag".to_string(),
            },
        ],
        platform_facts: json!({}),
    }
}

fn episodes() -> BTreeMap<String, Episode> {
    BTreeMap::from([(EP1.to_string(), episode())])
}

fn no_exists(_: &str) -> bool {
    false
}

fn no_search_results() -> HashSet<String> {
    HashSet::new()
}

fn base_mention() -> MentionSpan {
    MentionSpan {
        mention_id: "m1".to_string(),
        episode_id: EP1.to_string(),
        field_path: "title".to_string(),
        text: "塞尔达".to_string(),
        start: 1,
        end: 4,
        mention_type: "游戏名".to_string(),
        origin: "explicit".to_string(),
        proposed_entity_name: "塞尔达传说".to_string(),
        proposed_entity_type: "游戏".to_string(),
        entity_ref: "entity:e1".to_string(),
        confidence: 0.9,
    }
}

fn base_entity() -> EntityProposal {
    EntityProposal {
        local_id: "e1".to_string(),
        canonical_name: "塞尔达传说".to_string(),
        entity_type: "游戏".to_string(),
        aliases: vec![],
        description: String::new(),
        existing_entity_id: None,
        resolution: "NEW_ENTITY".to_string(),
        evidence_mention_ids: vec!["m1".to_string()],
        parent_entity_refs: vec![],
        confidence: 0.8,
    }
}

fn base_state() -> InterestStateProposal {
    InterestStateProposal {
        entity_ref: "entity:e1".to_string(),
        status: "无法判断".to_string(),
        preference: String::new(),
        aspects: vec![],
        rationale: String::new(),
        evidence_mention_ids: vec!["m1".to_string()],
        confidence: 0.5,
    }
}

fn valid_viewer() -> ViewerPerceptionSubmission {
    ViewerPerceptionSubmission {
        viewer_id: VIEWER.to_string(),
        profile_summary: "该观众近期集中关注塞尔达系列与开放世界玩法，优先新内容和互动攻略。"
            .to_string(),
        mentions: vec![base_mention()],
        entities: vec![base_entity()],
        relations: vec![],
        interest_states: vec![base_state()],
        content_preferences: vec![],
        recent_changes: vec![],
        hypotheses: vec![],
        conversation_openers: vec![],
        content_ideas: vec![],
        enrichment_targets: vec![],
        cautions: vec![],
        leads: vec![],
    }
}

fn check_viewer(sub: &ViewerPerceptionSubmission) -> Vec<String> {
    validate_viewer_submission(sub, VIEWER, &episodes(), &no_exists, &no_search_results())
}

fn has(errors: &[String], needle: &str) -> bool {
    errors.iter().any(|error| error.contains(needle))
}

// ---------------------------------------------------------------------------
// Python family 平移：基线 / ref 系错误
// ---------------------------------------------------------------------------

#[test]
fn valid_viewer_submission_passes() {
    assert_eq!(check_viewer(&valid_viewer()), Vec::<String>::new());
}

#[test]
fn viewer_id_mismatch_and_mention_dupes() {
    let mut sub = valid_viewer();
    sub.viewer_id = "other".to_string();
    assert!(has(&check_viewer(&sub), "viewer_id must be v1"));

    let mut sub = valid_viewer();
    sub.mentions.push(base_mention());
    assert!(has(&check_viewer(&sub), "duplicate mention_id: m1"));
}

/// 2026-08-05 生产事故复现（观众 77044362 graph_failed，467 秒感知 + 48 万 token 烧毁）：
/// `duplicate interest_state target` 曾穿透到 graph/build.rs 才判死。
/// 工具层必须先行拒收，且拒收文案指名冲突 ref，让模型合并/弃一后自纠。
#[test]
fn duplicate_interest_state_target_rejected_at_tool_layer() {
    // ①entity_ref 原样重复。
    let mut sub = valid_viewer();
    sub.interest_states.push(base_state());
    let errors = check_viewer(&sub);
    assert!(
        has(&errors, "interest states have duplicate target: entity:e1"),
        "同 ref 直抄必须拒: {errors:?}"
    );

    // ②SAME_AS 跨解析撞车：e1 映射到现有实体 entity:game:x，另一 state 直引同一现有实体。
    let exists_x = |id: &str| id == "entity:game:x";
    let mut sub = valid_viewer();
    sub.entities[0].resolution = "SAME_AS".to_string();
    sub.entities[0].existing_entity_id = Some("entity:game:x".to_string());
    let mut global_state = base_state();
    global_state.entity_ref = "entity:game:x".to_string();
    sub.interest_states.push(global_state);
    let errors =
        validate_viewer_submission(&sub, VIEWER, &episodes(), &exists_x, &no_search_results());
    assert!(
        has(
            &errors,
            "interest states have duplicate target: entity:game:x"
        ),
        "SAME_AS 撞同一现有实体必须拒: {errors:?}"
    );

    // ③对照：两条 state 指向不同对象必须放行。
    let mut sub = valid_viewer();
    let mut second = base_state();
    second.entity_ref = "entity:game:y".to_string();
    let exists_y = |id: &str| id == "entity:game:y";
    sub.interest_states.push(second);
    let errors =
        validate_viewer_submission(&sub, VIEWER, &episodes(), &exists_y, &no_search_results());
    assert!(
        !has(&errors, "duplicate target"),
        "不同 target 不得误伤: {errors:?}"
    );
}

#[test]
fn mention_episode_and_span_checks() {
    let mut sub = valid_viewer();
    sub.mentions[0].episode_id = "episode:v1:ep2".to_string();
    let errors = check_viewer(&sub);
    assert!(has(&errors, "unknown episode_id: episode:v1:ep2"));

    let mut sub = valid_viewer();
    sub.mentions[0].text = "不存在".to_string();
    let errors = check_viewer(&sub);
    assert!(has(&errors, "span mismatch"));

    let mut sub = valid_viewer();
    sub.mentions[0].field_path = "missing".to_string();
    let errors = check_viewer(&sub);
    assert!(has(&errors, "has no field missing"));
}

/// 修复 6a：平台字段上的 mention 必须 origin="platform"（单向；text 字段不限制）。
#[test]
fn origin_kind_binding() {
    let mut platform_mention = base_mention();
    platform_mention.mention_id = "m2".to_string();
    platform_mention.field_path = "tags[0]".to_string();
    platform_mention.text = "塞尔达".to_string();
    platform_mention.start = 0;
    platform_mention.end = 3;

    let mut sub = valid_viewer();
    sub.mentions.push(platform_mention.clone()); // origin 默认 explicit
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "mention m2 has origin \"explicit\" on platform field tags[0]"
    ));

    let mut sub = valid_viewer();
    let mut fixed = platform_mention;
    fixed.origin = "platform".to_string();
    sub.mentions.push(fixed);
    assert_eq!(check_viewer(&sub), Vec::<String>::new());

    // 单向规则：text 字段上的 platform origin 合法（tag 重现场景，seeds.rs 同型）。
    let mut sub = valid_viewer();
    sub.mentions[0].origin = "platform".to_string();
    assert_eq!(check_viewer(&sub), Vec::<String>::new());
}

#[test]
fn entity_resolution_family() {
    // duplicate local_id
    let mut sub = valid_viewer();
    sub.entities.push(base_entity());
    assert!(has(&check_viewer(&sub), "duplicate entity local_id: e1"));

    // unknown evidence mention + empty evidence
    let mut sub = valid_viewer();
    sub.entities[0].evidence_mention_ids = vec!["m9".to_string()];
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "entity e1 references unknown mentions: ['m9']"
    ));

    let mut sub = valid_viewer();
    sub.entities[0].evidence_mention_ids = vec![];
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "entity e1 must reference at least one grounded mention"
    ));

    // SAME_AS without existing id / unknown existing / forbidden id on other resolutions
    let mut sub = valid_viewer();
    sub.entities[0].resolution = "SAME_AS".to_string();
    assert!(has(
        &check_viewer(&sub),
        "entity e1 uses SAME_AS without existing_entity_id"
    ));

    let mut sub = valid_viewer();
    sub.entities[0].resolution = "SAME_AS".to_string();
    sub.entities[0].existing_entity_id = Some("ent9".to_string());
    assert!(has(
        &check_viewer(&sub),
        "entity e1 points to unknown existing entity"
    ));

    let mut sub = valid_viewer();
    sub.entities[0].existing_entity_id = Some("ent9".to_string());
    assert!(has(
        &check_viewer(&sub),
        "entity e1 cannot set existing_entity_id with NEW_ENTITY"
    ));

    let mut sub = valid_viewer();
    sub.entities[0].resolution = "UNCERTAIN".to_string();
    sub.entities[0].existing_entity_id = Some("ent9".to_string());
    assert!(has(
        &check_viewer(&sub),
        "entity e1 cannot set existing_entity_id with UNCERTAIN"
    ));

    // duplicate grounded NEW_ENTITY identity：同一 (type, mention 集合) 不得建两个新实体
    let mut sub = valid_viewer();
    let mut twin = base_entity();
    twin.local_id = "e2".to_string();
    sub.entities.push(twin);
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "entities e1 and e2 have duplicate grounded NEW_ENTITY identity"
    ));
}

/// 2026-08-05 生产事故复现：deepseek-v4-flash 真实提交 resolution="EXISTING"，
/// validators.rs 的 `_ => {}` 将其静默放行，93 万 token 烧完后在
/// graph::store::entities 才以 "unknown entity resolution decision" 判死。
/// 分层校验要求：未知 resolution 必须在 Tool Call 校验层拒收并回传合法取值表。
#[test]
fn unknown_resolution_rejected_at_tool_layer() {
    let mut sub = valid_viewer();
    sub.entities[0].resolution = "EXISTING".to_string();
    let errors = check_viewer(&sub);
    assert!(
        errors.iter().any(|e| e.contains("unknown resolution")
            && e.contains("EXISTING")
            && e.contains("SAME_AS")
            && e.contains("NEW_ENTITY")
            && e.contains("UNCERTAIN")),
        "unknown resolution 必须报出原值与合法取值表，实际: {errors:?}"
    );

    // 三个合法取值（大小写敏感、与图写入层一致）不得误伤为 unknown resolution。
    for allowed in ["SAME_AS", "NEW_ENTITY", "UNCERTAIN"] {
        let mut sub = valid_viewer();
        let mut entity = base_entity();
        entity.resolution = allowed.to_string();
        entity.existing_entity_id = None;
        sub.entities = vec![entity];
        let errors = check_viewer(&sub);
        assert!(
            !errors.iter().any(|e| e.contains("unknown resolution")),
            "{allowed} 不应触发 unknown resolution: {errors:?}"
        );
    }

    // 小写变体同样被拒（图写入层按全大写匹配，半放行也是穿透）。
    let mut sub = valid_viewer();
    sub.entities[0].resolution = "new_entity".to_string();
    let errors = check_viewer(&sub);
    assert!(
        errors.iter().any(|e| e.contains("unknown resolution")),
        "小写 new_entity 必须被拒: {errors:?}"
    );
}

#[test]
fn mention_entity_ref_and_known_refs() {
    // 未知 entity_ref（前缀查库也失败）
    let mut sub = valid_viewer();
    sub.mentions[0].entity_ref = "entity:e9".to_string();
    sub.interest_states = vec![];
    assert!(has(
        &check_viewer(&sub),
        "mention m1 has unknown entity_ref: entity:e9"
    ));

    // 无前缀裸 id 不查库（Python parity）
    let mut sub = valid_viewer();
    sub.mentions[0].entity_ref = "e9".to_string();
    sub.interest_states = vec![];
    assert!(has(
        &check_viewer(&sub),
        "mention m1 has unknown entity_ref: e9"
    ));

    // parent_ref 未知
    let mut sub = valid_viewer();
    sub.entities[0].parent_entity_refs = vec!["entity:e9".to_string()];
    assert!(has(
        &check_viewer(&sub),
        "entity e1 has unknown parent_ref: entity:e9"
    ));

    // entity_exists 回调为真时 entity:e9 可用
    let mut sub = valid_viewer();
    sub.entities[0].parent_entity_refs = vec!["entity:e9".to_string()];
    let errors = validate_viewer_submission(
        &sub,
        VIEWER,
        &episodes(),
        &|id| id == "entity:e9",
        &no_search_results(),
    );
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn relation_and_state_checks() {
    // UNCERTAIN 本地实体不得做关系/状态的形式端点（带后缀文案）
    let mut sub = valid_viewer();
    sub.entities[0].resolution = "UNCERTAIN".to_string();
    sub.interest_states = vec![];
    sub.relations = vec![RelationProposal {
        subject_ref: "viewer:self".to_string(),
        predicate: "ABOUT".to_string(),
        object_ref: "entity:e1".to_string(),
        interpretation: String::new(),
        evidence_mention_ids: vec!["m1".to_string()],
        confidence: 0.7,
    }];
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "relation has unknown object_ref: entity:e1 (UNCERTAIN entities are not formal)"
    ));

    // 空证据 + 未知 mention
    let mut sub = valid_viewer();
    sub.relations = vec![RelationProposal {
        subject_ref: "viewer:self".to_string(),
        predicate: "ABOUT".to_string(),
        object_ref: "entity:e1".to_string(),
        interpretation: String::new(),
        evidence_mention_ids: vec![],
        confidence: 0.7,
    }];
    assert!(has(
        &check_viewer(&sub),
        "relation ABOUT must reference grounded mentions"
    ));

    // interest state：空证据
    let mut sub = valid_viewer();
    sub.interest_states[0].evidence_mention_ids = vec![];
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "interest state entity:e1 must reference grounded mentions"
    ));

    // interest state 指向 UNCERTAIN 本地实体
    let mut sub = valid_viewer();
    sub.entities[0].resolution = "UNCERTAIN".to_string();
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "interest state has unknown entity_ref: entity:e1 (UNCERTAIN entities are not formal)"
    ));
}

#[test]
fn action_checks_and_search_run_isolation() {
    let mut sub = valid_viewer();
    sub.conversation_openers = vec![ViewerAction {
        title: "  ".to_string(),
        detail: String::new(),
        evidence_mention_ids: vec!["m9".to_string()],
        search_result_ids: vec![],
        observation_metrics: vec![],
        risk: String::new(),
    }];
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "conversation_openers action title cannot be empty"
    ));
    assert!(has(
        &errors,
        "conversation_openers action references unknown mentions: ['m9']"
    ));

    // 修复 6c：search_result_ids 按运行隔离——别的运行搜到的 id 本运行不可用
    let mut sub = valid_viewer();
    sub.content_ideas = vec![ViewerAction {
        title: "试做一期攻略".to_string(),
        detail: String::new(),
        evidence_mention_ids: vec!["m1".to_string()],
        search_result_ids: vec!["sr-other-run".to_string()],
        observation_metrics: vec![],
        risk: String::new(),
    }];
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "content_ideas action references unknown search results: ['sr-other-run']"
    ));

    // 本运行注册表内的 id 通过
    let known = HashSet::from(["sr1".to_string()]);
    sub.content_ideas[0].search_result_ids = vec!["sr1".to_string()];
    let errors = validate_viewer_submission(&sub, VIEWER, &episodes(), &no_exists, &known);
    assert_eq!(errors, Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// 修复 8（viewer）+ leads
// ---------------------------------------------------------------------------

#[test]
fn fix8_viewer_placeholder_and_empty_submission() {
    let mut sub = valid_viewer();
    sub.profile_summary = "测试".to_string();
    assert!(has(
        &check_viewer(&sub),
        "profile_summary is placeholder text"
    ));

    let mut sub = valid_viewer();
    sub.profile_summary = "太短".to_string();
    assert!(has(
        &check_viewer(&sub),
        "profile_summary is too short to be a real analysis"
    ));

    // 双侧空：hypotheses 只有单字符条目（不实质）→ 拒
    // （mention.entity_ref 只允许本地实体或 entity:+图实体，本用图实体回调）
    let graph_exists = |id: &str| id == "entity:ent9";
    let mut sub = valid_viewer();
    sub.entities = vec![];
    sub.interest_states = vec![];
    sub.mentions[0].entity_ref = "entity:ent9".to_string();
    sub.hypotheses = vec!["a".to_string()];
    let errors = validate_viewer_submission(
        &sub,
        VIEWER,
        &episodes(),
        &graph_exists,
        &no_search_results(),
    );
    assert!(has(
        &errors,
        "viewer submission has empty entities and interest_states; provide hypotheses, cautions, content_preferences, recent_changes, or enrichment_targets"
    ));

    // 实质 hypothesis 放行
    sub.hypotheses = vec!["观众可能在等王国之泪相关二创".to_string()];
    let errors = validate_viewer_submission(
        &sub,
        VIEWER,
        &episodes(),
        &graph_exists,
        &no_search_results(),
    );
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn viewer_leads_validation() {
    let ok_lead = |lead_type: &str, locator: &str| Lead {
        lead_type: lead_type.to_string(),
        locator: locator.to_string(),
        motivation: "观众集中提及".to_string(),
        expected_signal: "下一期视频互动".to_string(),
        priority: "p1".to_string(),
        evidence_ids: vec!["m1".to_string(), "e1".to_string()],
    };

    let mut sub = valid_viewer();
    sub.leads = vec![ok_lead("search", "塞尔达 续作")];
    assert_eq!(check_viewer(&sub), Vec::<String>::new());

    let mut sub = valid_viewer();
    sub.leads = vec![ok_lead("watch", "x")];
    assert!(has(
        &check_viewer(&sub),
        "leads[0] type must be one of ['search', 'creator', 'video', 'room']"
    ));

    let mut sub = valid_viewer();
    sub.leads = vec![ok_lead("creator", "abc")];
    assert!(has(
        &check_viewer(&sub),
        "leads[0] locator must be numeric for type creator"
    ));

    let mut sub = valid_viewer();
    sub.leads = vec![ok_lead("video", "1xx411c7mD")];
    assert!(has(
        &check_viewer(&sub),
        "leads[0] locator must be a BV id for type video"
    ));
    let mut sub = valid_viewer();
    sub.leads = vec![ok_lead("video", "BV1xx411c7mD")];
    assert_eq!(check_viewer(&sub), Vec::<String>::new());

    let mut sub = valid_viewer();
    let mut lead = ok_lead("room", "21452505");
    lead.motivation = "  ".to_string();
    sub.leads = vec![lead];
    assert!(has(
        &check_viewer(&sub),
        "leads[0] motivation cannot be empty"
    ));

    let mut sub = valid_viewer();
    let mut lead = ok_lead("search", "x");
    lead.evidence_ids = vec!["m9".to_string()];
    sub.leads = vec![lead];
    let errors = check_viewer(&sub);
    assert!(has(&errors, "leads[0] references unknown evidence: ['m9']"));

    // UNCERTAIN 实体不得驱动线索
    let mut sub = valid_viewer();
    let mut uncertain = base_entity();
    uncertain.local_id = "e2".to_string();
    uncertain.resolution = "UNCERTAIN".to_string();
    sub.entities.push(uncertain);
    let mut lead = ok_lead("search", "x");
    lead.evidence_ids = vec!["e2".to_string()];
    sub.leads = vec![lead];
    let errors = check_viewer(&sub);
    assert!(has(
        &errors,
        "leads[0] must not be driven by UNCERTAIN entity: e2"
    ));
}

// ---------------------------------------------------------------------------
// audience 侧：基线 / 五组 unknown / 修复 1 / 修复 6b / 修复 8 / leads
// ---------------------------------------------------------------------------

fn audience_sets() -> (
    HashSet<String>,
    HashSet<String>,
    HashSet<String>,
    HashSet<String>,
) {
    (
        HashSet::from(["v1".to_string()]),
        HashSet::from(["ent1".to_string()]),
        HashSet::from(["m1".to_string()]),
        HashSet::from(["sr1".to_string()]),
    )
}

fn valid_audience() -> AudienceSituationSubmission {
    AudienceSituationSubmission {
        executive_summary: "观众围绕塞尔达系列形成单一高粘社区，近期对新作内容需求上升。"
            .to_string(),
        front_brief: Default::default(),
        audience_structure: vec![],
        interest_graph: vec![GraphInterestItem {
            entity_id: "ent1".to_string(),
            entity: "塞尔达传说".to_string(),
            entity_type: "游戏".to_string(),
            parent_entities: vec![],
            angles: vec![],
            viewer_ids: vec!["v1".to_string()],
            status: "无法判断".to_string(),
            confidence: 0.6,
            evidence_summary: String::new(),
            evidence_mention_ids: vec!["m1".to_string()],
        }],
        communities: vec![],
        situations: vec![],
        content_opportunities: vec![],
        individual_highlights: vec![],
        content_calendar: vec![],
        data_gaps: vec![],
        safety_notes: vec![],
        leads: vec![],
    }
}

fn check_audience(sub: &AudienceSituationSubmission) -> Vec<String> {
    let (viewers, entities, mentions, searches) = audience_sets();
    validate_audience_submission(sub, &viewers, &entities, &mentions, &searches)
}

#[test]
fn valid_audience_submission_passes() {
    assert_eq!(check_audience(&valid_audience()), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// front_brief 校验钉：结构规则 + 沉默可呈现（空 sentences 恒合法）。
// 存在性闭包钉在 pipeline_run.rs 集成层（specs 终局闭包走 graph references）。
// ---------------------------------------------------------------------------

fn brief_sentence(text: &str, refs: Vec<&str>, range: Option<(&str, &str)>) -> BriefSentence {
    BriefSentence {
        text: text.to_string(),
        episode_refs: refs.into_iter().map(str::to_string).collect(),
        coverage_time_range: range.map(|(f, t)| [f.to_string(), t.to_string()]),
    }
}

/// 沉默可呈现：front_brief 缺席/空句集恒合法（valid_audience 默认即空）。
#[test]
fn brief_silence_is_presentable() {
    let sub = valid_audience();
    assert!(
        sub.front_brief.sentences.is_empty(),
        "valid_audience 默认沉默"
    );
    assert_eq!(check_audience(&sub), Vec::<String>::new());
}

/// 合规简报通过结构校验：句号、带出处、覆盖时段有序。
#[test]
fn brief_sentence_wellformed_passes() {
    let mut sub = valid_audience();
    sub.front_brief.sentences = vec![brief_sentence(
        "《塞尔达》内容需求在最近一周显著升温。",
        vec!["episode:v1:ep1"],
        Some(("2026-08-01", "2026-08-04T00:00:00+00:00")),
    )];
    assert_eq!(check_audience(&sub), Vec::<String>::new());
}

/// 无出处之句必拒（哲学桩一），哪怕 refs 是空数组也拒。
#[test]
fn brief_sentence_without_refs_rejected() {
    let mut sub = valid_audience();
    sub.front_brief.sentences = vec![brief_sentence("无出处的一句话。", vec![], None)];
    let errors = check_audience(&sub);
    assert!(
        has(
            &errors,
            "front_brief.sentences[0] must reference at least one episode"
        ),
        "{errors:?}"
    );
}

/// 空文本、超长文本、乱序/坏形 coverage 三类结构缺陷各自具名。
#[test]
fn brief_structural_defects_named() {
    let mut sub = valid_audience();
    sub.front_brief.sentences = vec![
        brief_sentence("   ", vec!["episode:v1:ep1"], None),
        brief_sentence(&"长".repeat(501), vec!["episode:v1:ep1"], None),
        brief_sentence(
            "时段乱序",
            vec!["episode:v1:ep1"],
            Some(("2026-08-04", "2026-08-01")),
        ),
        brief_sentence(
            "坏时段",
            vec!["episode:v1:ep1"],
            Some(("not-a-date", "2026-08-01")),
        ),
    ];
    let errors = check_audience(&sub);
    assert!(
        has(&errors, "front_brief.sentences[0] text cannot be empty"),
        "{errors:?}"
    );
    assert!(
        has(&errors, "front_brief.sentences[1] text exceeds 500 chars"),
        "{errors:?}"
    );
    assert!(
        has(
            &errors,
            "front_brief.sentences[2] coverage_time_range from must be <= to"
        ),
        "{errors:?}"
    );
    assert!(
        has(
            &errors,
            "front_brief.sentences[3] coverage_time_range must be ISO date/datetime strings"
        ),
        "{errors:?}"
    );
}

/// 句数超帽具名拒绝（12 之上皆拒）。
#[test]
fn brief_sentence_cap_enforced() {
    let mut sub = valid_audience();
    sub.front_brief.sentences = (0..13)
        .map(|i| brief_sentence(&format!("第{i}句"), vec!["episode:v1:ep1"], None))
        .collect();
    let errors = check_audience(&sub);
    assert!(
        has(&errors, "front_brief.sentences exceeds cap 12"),
        "{errors:?}"
    );
}

#[test]
fn audience_group_unknowns() {
    let mut sub = valid_audience();
    sub.interest_graph[0].viewer_ids = vec!["v9".to_string()];
    sub.interest_graph[0].entity_id = "ent9".to_string();
    sub.interest_graph[0].evidence_mention_ids = vec!["m9".to_string()];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "interest_graph[0] references unknown viewers: ['v9']"
    ));
    assert!(has(
        &errors,
        "interest_graph[0] references unknown entities: ['ent9']"
    ));
    assert!(has(
        &errors,
        "interest_graph[0] references unknown mentions: ['m9']"
    ));

    let mut sub = valid_audience();
    sub.content_opportunities = vec![ContentOpportunity {
        title: "空".to_string(),
        entity_id: String::new(),
        entity: String::new(),
        why_now: String::new(),
        why_fit: String::new(),
        audience_ids: vec!["v9".to_string()],
        format: String::new(),
        run_of_show: vec![],
        talking_points: vec![],
        evidence_mention_ids: vec!["m1".to_string()],
        search_result_ids: vec!["sr9".to_string()],
        confidence: "低".to_string(),
        observation_metrics: vec![],
        caveats: vec![],
    }];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "content_opportunities[0] references unknown viewers: ['v9']"
    ));
    assert!(has(
        &errors,
        "content_opportunities[0] references unknown search results: ['sr9']"
    ));
}

/// 修复 1：search_result_ids 不再豁免 mention 证据（S0/Python 逃逸口关闭回归）。
#[test]
fn fix1_opportunity_requires_mentions() {
    let mut sub = valid_audience();
    sub.content_opportunities = vec![ContentOpportunity {
        title: "塞尔达新作企划".to_string(),
        entity_id: "ent1".to_string(),
        entity: String::new(),
        why_now: String::new(),
        why_fit: String::new(),
        audience_ids: vec!["v1".to_string()],
        format: String::new(),
        run_of_show: vec![],
        talking_points: vec![],
        evidence_mention_ids: vec![],
        search_result_ids: vec!["sr1".to_string()],
        confidence: "低".to_string(),
        observation_metrics: vec![],
        caveats: vec![],
    }];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "content_opportunities[0] must reference grounded mentions"
    ));
}

/// 修复 6b：空串 entity_id 不再被静默放过；interest_graph 空 entity_id 显式拒。
#[test]
fn fix6b_audience_empty_entity_ids_rejected() {
    let mut sub = valid_audience();
    sub.interest_graph[0].entity_id = String::new();
    let errors = check_audience(&sub);
    assert!(has(&errors, "interest_graph[0] entity_id cannot be empty"));

    let mut sub = valid_audience();
    let mut community = live_core::models::AudienceCommunity {
        name: "塞尔达讨论组".to_string(),
        description: String::new(),
        viewer_ids: vec!["v1".to_string()],
        entity_ids: vec![String::new()],
        entities: vec![],
        shared_angles: vec![],
        evidence_mention_ids: vec!["m1".to_string()],
        confidence: 0.5,
    };
    sub.communities = vec![community.clone()];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "communities[0] references unknown entities: ['']"
    ));

    // content_opportunity 的 entity_id 仍是选填：空串合法
    community.entity_ids = vec![];
    let mut sub = valid_audience();
    sub.interest_graph = vec![];
    sub.communities = vec![community];
    sub.content_opportunities = vec![ContentOpportunity {
        title: "不挂实体的机会".to_string(),
        entity_id: String::new(),
        entity: String::new(),
        why_now: String::new(),
        why_fit: String::new(),
        audience_ids: vec!["v1".to_string()],
        format: String::new(),
        run_of_show: vec![],
        talking_points: vec![],
        evidence_mention_ids: vec!["m1".to_string()],
        search_result_ids: vec![],
        confidence: "低".to_string(),
        observation_metrics: vec![],
        caveats: vec![],
    }];
    assert_eq!(check_audience(&sub), Vec::<String>::new());
}

#[test]
fn fix8_audience_placeholder_and_empty_sections() {
    let mut sub = valid_audience();
    sub.executive_summary = "summary".to_string();
    assert!(has(
        &check_audience(&sub),
        "executive_summary is placeholder text"
    ));

    let mut sub = valid_audience();
    sub.executive_summary = "短".to_string();
    assert!(has(
        &check_audience(&sub),
        "executive_summary is too short to be a real analysis"
    ));

    let mut sub = valid_audience();
    sub.interest_graph = vec![];
    sub.audience_structure = vec!["a".to_string()]; // 单字符不实质
    assert!(has(
        &check_audience(&sub),
        "at least one audience section must be non-empty"
    ));

    let mut sub = valid_audience();
    sub.interest_graph = vec![];
    sub.audience_structure = vec!["单一高粘社区".to_string()];
    assert_eq!(check_audience(&sub), Vec::<String>::new());
}

#[test]
fn audience_leads_validation() {
    let mut sub = valid_audience();
    sub.leads = vec![Lead {
        lead_type: "creator".to_string(),
        locator: "12345".to_string(),
        motivation: "同好创作者".to_string(),
        expected_signal: "联动话题".to_string(),
        priority: "p2".to_string(),
        evidence_ids: vec!["ent1".to_string(), "m1".to_string()],
    }];
    assert_eq!(check_audience(&sub), Vec::<String>::new());

    let mut sub = valid_audience();
    sub.leads = vec![Lead {
        lead_type: "creator".to_string(),
        locator: "12345".to_string(),
        motivation: "m".to_string(),
        expected_signal: "s".to_string(),
        priority: "p2".to_string(),
        evidence_ids: vec!["e9".to_string()],
    }];
    let errors = check_audience(&sub);
    assert!(has(&errors, "leads[0] references unknown evidence: ['e9']"));
}

// ---------------------------------------------------------------------------
// 上限硬拒 / 占位词实质 / Some("")arity / audience 循环黑盲补钉
// ---------------------------------------------------------------------------

#[test]
fn summary_over_max_chars_rejected() {
    let mut sub = valid_viewer();
    sub.profile_summary = "长".repeat(100_001);
    let errors = check_viewer(&sub);
    assert!(has(&errors, "profile_summary exceeds max length 100000"));

    let mut sub = valid_audience();
    sub.executive_summary = "长".repeat(100_001);
    let errors = check_audience(&sub);
    assert!(has(&errors, "executive_summary exceeds max length 100000"));
}

/// 评审3-M2：占位词放进栏目串不再算"实质内容"
#[test]
fn placeholder_words_do_not_count_as_substantive() {
    let mut sub = valid_audience();
    sub.interest_graph = vec![];
    sub.audience_structure = vec!["测试".to_string()];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "at least one audience section must be non-empty"
    ));

    let graph_exists = |id: &str| id == "entity:ent9";
    let mut sub = valid_viewer();
    sub.entities = vec![];
    sub.interest_states = vec![];
    sub.mentions[0].entity_ref = "entity:ent9".to_string();
    sub.hypotheses = vec!["占位".to_string()];
    let errors = validate_viewer_submission(
        &sub,
        VIEWER,
        &episodes(),
        &graph_exists,
        &no_search_results(),
    );
    assert!(has(
        &errors,
        "viewer submission has empty entities and interest_states; provide hypotheses, cautions, content_preferences, recent_changes, or enrichment_targets"
    ));
}

/// 评审3-M1：Some("") 在 NEW_ENTITY/UNCERTAIN 臂不触发（Python truthiness parity）；
/// SAME_AS 空串仍走 "without existing_entity_id"。
#[test]
fn empty_string_existing_id_parity() {
    let mut sub = valid_viewer();
    sub.entities[0].existing_entity_id = Some(String::new());
    assert_eq!(check_viewer(&sub), Vec::<String>::new());

    let mut sub = valid_viewer();
    sub.entities[0].resolution = "SAME_AS".to_string();
    sub.entities[0].existing_entity_id = Some(String::new());
    assert!(has(
        &check_viewer(&sub),
        "entity e1 uses SAME_AS without existing_entity_id"
    ));

    let mut sub = valid_viewer();
    sub.entities[0].resolution = "UNCERTAIN".to_string();
    sub.entities[0].existing_entity_id = Some(String::new());
    sub.interest_states = vec![];
    sub.relations = vec![];
    assert_eq!(check_viewer(&sub), Vec::<String>::new());
}

/// 评审5-M3：audience 的 situations / individual_highlights / content_calendar
/// 三个循环此前零测试。
#[test]
fn audience_situations_highlights_calendar_unknowns() {
    use live_core::models::{ContentCalendarItem, IndividualHighlight, SituationItem};

    let mut sub = valid_audience();
    sub.situations = vec![SituationItem {
        title: "某态势".to_string(),
        status: "上升".to_string(),
        description: String::new(),
        entity_ids: vec!["ent9".to_string()],
        entities: vec![],
        viewer_ids: vec!["v9".to_string()],
        trigger_events: vec![],
        evidence_mention_ids: vec!["m9".to_string()],
        confidence: 0.5,
        recommended_investigation: vec![],
    }];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "situations[0] references unknown viewers: ['v9']"
    ));
    assert!(has(
        &errors,
        "situations[0] references unknown entities: ['ent9']"
    ));
    assert!(has(
        &errors,
        "situations[0] references unknown mentions: ['m9']"
    ));

    let mut sub = valid_audience();
    sub.individual_highlights = vec![IndividualHighlight {
        viewer_id: "v9".to_string(),
        insight: "x".to_string(),
        opportunity: String::new(),
        evidence_mention_ids: vec!["m9".to_string()],
    }];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "individual_highlights[0] references unknown viewers: ['v9']"
    ));
    assert!(has(
        &errors,
        "individual_highlights[0] references unknown mentions: ['m9']"
    ));

    // empty 证据必拒（required=true）
    let mut sub = valid_audience();
    sub.individual_highlights = vec![IndividualHighlight {
        viewer_id: "v1".to_string(),
        insight: "x".to_string(),
        opportunity: String::new(),
        evidence_mention_ids: vec![],
    }];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "individual_highlights[0] must reference grounded mentions"
    ));

    let mut sub = valid_audience();
    sub.content_calendar = vec![ContentCalendarItem {
        session: "周六档".to_string(),
        theme: "塞尔达".to_string(),
        target_viewers: vec!["v9".to_string()],
        goal: String::new(),
        validation_signal: String::new(),
    }];
    let errors = check_audience(&sub);
    assert!(has(
        &errors,
        "content_calendar[0] references unknown viewers: ['v9']"
    ));
}

/// 只有内容偏好/近期变化/深挖目标的实质提交不得被误判空——
/// 修前守卫只认 entities+interest_states+hypotheses+cautions 四件套；
/// content_preferences/recent_changes/enrichment_targets 三键的合法内容零计起得到空拒。
#[test]
fn substantive_content_fields_not_misjudged_empty() {
    // 分设三例：任一文本键单独撑起实质即可过闸（不加 entities/states/mentions）
    type Mutator = Box<dyn Fn(&mut ViewerPerceptionSubmission)>;
    let cases: Vec<(&str, Mutator)> = vec![
        (
            "content_preferences",
            Box::new(|s: &mut ViewerPerceptionSubmission| {
                s.content_preferences = vec!["喜欢开场唱歌".to_string()];
            }),
        ),
        (
            "recent_changes",
            Box::new(|s: &mut ViewerPerceptionSubmission| {
                s.recent_changes = vec!["昨晚切了播间风格".to_string()];
            }),
        ),
        (
            "enrichment_targets",
            Box::new(|s: &mut ViewerPerceptionSubmission| {
                s.enrichment_targets = vec!["想补 dynamic_note".to_string()];
            }),
        ),
    ];
    for (name, mutate) in &cases {
        let mut submission = valid_viewer();
        submission.entities = vec![];
        submission.interest_states = vec![];
        submission.mentions = vec![];
        submission.hypotheses = vec![];
        submission.cautions = vec![];
        mutate(&mut submission);
        let errors = check_viewer(&submission);
        assert!(
            !has(&errors, "empty entities and interest_states"),
            "{name} 是实质内容，不得再被空拒: {errors:?}"
        );
    }
    // 对照：全空即真拒——不放松精度
    let mut empty = valid_viewer();
    empty.entities = vec![];
    empty.interest_states = vec![];
    empty.mentions = vec![];
    empty.hypotheses = vec![];
    empty.cautions = vec![];
    let errors = check_viewer(&empty);
    assert!(
        has(&errors, "empty entities and interest_states"),
        "全空必须照拒: {errors:?}"
    );
}
