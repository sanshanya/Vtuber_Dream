//! 图的受控写入（移植 Python `graph.py` 下半部：ingest / apply_*）。
//!
//! 体积备书：超 500 线 = 三写族共享 savepoint/confidence/区间幂等语义，
//! 是 Python graph.py 下半镜像。缝 = 按 apply_* 对象分卷；parity 对照面未破前不动。
//!
//! 与 Python 的行为差异**仅限设计文档 v6 明示的升级**：
//! 1. TARGETS/ABOUT 边必带 confidence（ConfidenceWord → 数值，见 models.rs）；
//! 2. INTERESTED_IN 幂等（§8.2）：同内容重跑保留原 interval（edge_id 与
//!    valid_from 不变，仅合并 evidence/confidence）；内容变化才关闭旧区间开新区间；
//! 3. 不可解的 node ref 直接报错（Python 沉默写入字符串 "None"，v6 失败显式化）。
//!
//! 幂等注记：`apply_*` 在 SAVEPOINT 内执行，失败整体回滚，重跑不产生二次写入。

use serde_json::{Map, Value};

use crate::episodes::{Episode, hash_parts, json_canon, py_str_int};
use crate::graph::store::{Result, Store, StoreError, mention_id_of, with_savepoint};
use crate::models::{
    AudienceSituationSubmission, ViewerPerceptionSubmission, confidence_word_score,
};

// ---------------------------------------------------------------------------
// Episode 平台事实落图：OBSERVED / CREATED_BY / TAGGED_AS / IN_PARTITION
// ---------------------------------------------------------------------------

/// entity_id("bilibili_tag"|"bilibili_category", name)；creator 优先 platform raw_id。
pub(crate) fn entity_id(entity_type: &str, name: &str) -> String {
    format!(
        "entity:{}:{}",
        crate::episodes::safe_type(entity_type),
        hash_parts(&[crate::episodes::norm(name)], 18)
    )
}

pub(crate) fn creator_entity_id(raw_id: &str, name: &str) -> String {
    if raw_id.is_empty() {
        entity_id("creator", name)
    } else {
        format!("entity:creator:{raw_id}")
    }
}

pub fn ingest_platform_facts(store: &Store, run_id: &str, episode: &Episode) -> Result<()> {
    store.upsert_episode(episode)?;
    let mut node_props = Map::new();
    node_props.insert(
        "viewer_id".to_string(),
        Value::String(episode.viewer_id.clone()),
    );
    node_props.insert("source".to_string(), Value::String(episode.source.clone()));
    node_props.insert(
        "event_type".to_string(),
        Value::String(episode.event_type.clone()),
    );
    node_props.insert("url".to_string(), Value::String(episode.url.clone()));
    node_props.insert(
        "published_at".to_string(),
        Value::String(episode.published_at.clone()),
    );
    let node_name = if !episode.title.is_empty() {
        episode.title.clone()
    } else if !episode.source.is_empty() {
        episode.source.clone()
    } else {
        episode.episode_id.clone()
    };
    store.upsert_node(
        &episode.episode_id,
        "Episode",
        &node_name,
        &Value::Object(node_props),
        "platform_fact",
        Some(&episode.observed_at),
    )?;
    store.upsert_edge(
        &format!("viewer:{}", episode.viewer_id),
        "OBSERVED",
        &episode.episode_id,
        &serde_json::json!({"source": episode.source}),
        "platform_fact",
        Some(1.0),
        std::slice::from_ref(&episode.episode_id),
        run_id,
        None,
    )?;

    let facts = episode
        .platform_facts
        .as_object()
        .cloned()
        .unwrap_or_default();
    // (predicate, entity_id, name, entity_type, properties)
    let mut linked: Vec<(String, String, String, String, Value)> = Vec::new();
    let creator_name = facts
        .get("creator_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !creator_name.is_empty() {
        let creator_id = facts
            .get("creator_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let props = if creator_id.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({"platform_id": creator_id})
        };
        linked.push((
            "CREATED_BY".to_string(),
            creator_entity_id(&creator_id, &creator_name),
            creator_name,
            "creator".to_string(),
            props,
        ));
    }
    if let Some(Value::Array(tags)) = facts.get("tags") {
        for tag in tags {
            if let Some(tag_name_raw) = tag.as_str() {
                let tag_name = tag_name_raw.trim().to_string();
                if !tag_name.is_empty() {
                    linked.push((
                        "TAGGED_AS".to_string(),
                        entity_id("bilibili_tag", &tag_name),
                        tag_name.clone(),
                        "bilibili_tag".to_string(),
                        serde_json::json!({"platform_value": tag_name}),
                    ));
                }
            }
        }
    }
    if let Some(Value::Object(category)) = facts.get("platform_category") {
        let category_name = category
            .get("name")
            .map(crate::episodes::py_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !category_name.is_empty() {
            // Python str(category.get("id") or "")：数字 id 转十进制字符串。
            let platform_category_id = category
                .get("id")
                .map(crate::episodes::py_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let fact_id = if platform_category_id.is_empty() {
                entity_id("bilibili_category", &category_name)
            } else {
                format!("entity:bilibili_category:{platform_category_id}")
            };
            linked.push((
                "IN_PARTITION".to_string(),
                fact_id,
                category_name,
                "bilibili_category".to_string(),
                serde_json::json!({"platform_id": platform_category_id}),
            ));
        }
    }
    for (predicate, fact_id, name, fact_type, properties) in linked {
        store.upsert_platform_entity(&fact_id, &name, &fact_type, &properties)?;
        store.upsert_edge(
            &episode.episode_id,
            &predicate,
            &fact_id,
            &serde_json::json!({}),
            "platform_fact",
            Some(1.0),
            std::slice::from_ref(&episode.episode_id),
            run_id,
            None,
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Room spine（v6 §8.1：主播身份 + GUARD_OF / OWNS_ROOM 平台事实边）
// ---------------------------------------------------------------------------

/// 主播脊柱：主播节点 = viewer:{streamer_uid}（身份靠边表达）、房间节点、
/// OWNS_ROOM（主播→房间）与 GUARD_OF（观众→主播）平台事实边。
/// 证据数组为空（名单来自 API 而非 Episode）；collector（M2）在 collect 阶段调用。
pub fn ingest_room_spine(
    store: &Store,
    run_id: &str,
    room_id: &str,
    streamer_uid: &str,
    guard_viewer_ids: &[String],
) -> Result<()> {
    let streamer_node = format!("viewer:{streamer_uid}");
    store.upsert_node(
        &streamer_node,
        "Viewer",
        streamer_uid,
        &serde_json::json!({"viewer_id": streamer_uid}),
        "platform_fact",
        None,
    )?;
    let room_node = format!("room:{room_id}");
    store.upsert_node(
        &room_node,
        "Room",
        room_id,
        &serde_json::json!({"room_id": room_id}),
        "platform_fact",
        None,
    )?;
    store.upsert_edge(
        &streamer_node,
        "OWNS_ROOM",
        &room_node,
        &serde_json::json!({}),
        "platform_fact",
        Some(1.0),
        &[],
        run_id,
        None,
    )?;
    apply_guard_edges(store, run_id, &streamer_node, guard_viewer_ids)
}

fn apply_guard_edges(
    store: &Store,
    run_id: &str,
    streamer_node: &str,
    guard_viewer_ids: &[String],
) -> Result<()> {
    for viewer_id in guard_viewer_ids {
        let guard_node = format!("viewer:{viewer_id}");
        store.upsert_node(
            &guard_node,
            "Viewer",
            viewer_id,
            &serde_json::json!({"viewer_id": viewer_id}),
            "platform_fact",
            None,
        )?;
        store.upsert_edge(
            &guard_node,
            "GUARD_OF",
            streamer_node,
            &serde_json::json!({}),
            "platform_fact",
            Some(1.0),
            &[],
            run_id,
            None,
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// node ref 解析
// ---------------------------------------------------------------------------

/// viewer:self / entity:<local_id> / <local_id> / episode:* / entity:*。
/// 不可解 → None（v6：调用方显式报错）。
pub(crate) fn node_ref(
    reference: &str,
    local_entities: &std::collections::HashMap<String, String>,
    viewer_id: &str,
) -> Option<String> {
    if reference == "viewer:self" {
        return Some(format!("viewer:{viewer_id}"));
    }
    if let Some(local) = reference.strip_prefix("entity:")
        && let Some(resolved) = local_entities.get(local)
    {
        return Some(resolved.clone());
    }
    if let Some(resolved) = local_entities.get(reference) {
        return Some(resolved.clone());
    }
    if reference.starts_with("episode:") || reference.starts_with("entity:") {
        return Some(reference.to_string());
    }
    None
}

/// 证据引用解析：AI 引用的 mention 必须存在于本次提交映射中。
/// 未知引用 = 协议违规，显式报错（不静默丢弃，§6 校验规则）。
fn resolve_evidence(
    refs: &[String],
    mention_id_map: &std::collections::HashMap<String, String>,
) -> Result<Vec<String>> {
    refs.iter()
        .map(|item| {
            mention_id_map
                .get(item)
                .cloned()
                .ok_or_else(|| StoreError::Repo(format!("unresolvable evidence mention: {item}")))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 观众提交应用
// ---------------------------------------------------------------------------

pub fn apply_viewer_submission(
    store: &Store,
    run_id: &str,
    viewer_name: &str,
    episodes: &[Episode],
    output: &ViewerPerceptionSubmission,
) -> Result<()> {
    let viewer_id = output.viewer_id.clone();
    with_savepoint("viewer_apply", store, || {
        apply_viewer_inner(store, run_id, viewer_name, episodes, output, &viewer_id)
    })
}

fn apply_viewer_inner(
    store: &Store,
    run_id: &str,
    viewer_name: &str,
    episodes: &[Episode],
    output: &ViewerPerceptionSubmission,
    viewer_id: &str,
) -> Result<()> {
    let shown_name = if viewer_name.is_empty() {
        viewer_id
    } else {
        viewer_name
    };
    store.upsert_node(
        &format!("viewer:{viewer_id}"),
        "Viewer",
        shown_name,
        &serde_json::json!({"viewer_id": viewer_id}),
        "platform_fact",
        None,
    )?;
    for episode in episodes {
        ingest_platform_facts(store, run_id, episode)?;
    }

    let mention_id_map: std::collections::HashMap<String, String> = output
        .mentions
        .iter()
        .map(|mention| {
            (
                mention.mention_id.clone(),
                mention_id_of(viewer_id, mention),
            )
        })
        .collect();
    let mut local_entities: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut resolution_by_local: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for proposal in &output.entities {
        let evidence = resolve_evidence(&proposal.evidence_mention_ids, &mention_id_map)?;
        let (resolved, decision) = store.resolve_entity(proposal, run_id, viewer_id, &evidence)?;
        local_entities.insert(proposal.local_id.clone(), resolved);
        resolution_by_local.insert(proposal.local_id.clone(), decision);
    }

    for mention in &output.mentions {
        let resolved = node_ref(&mention.entity_ref, &local_entities, viewer_id)
            .filter(|candidate| store.entity_exists(candidate).unwrap_or(false));
        let local_id = mention
            .entity_ref
            .strip_prefix("entity:")
            .unwrap_or(&mention.entity_ref);
        let decision = resolution_by_local
            .get(local_id)
            .cloned()
            .unwrap_or_else(|| "SAME_AS".to_string());
        store.upsert_mention(mention, viewer_id, run_id, resolved.as_deref(), &decision)?;
    }

    for proposal in &output.entities {
        let source = local_entities[&proposal.local_id].clone();
        if !store.entity_exists(&source)? {
            continue;
        }
        let evidence = resolve_evidence(&proposal.evidence_mention_ids, &mention_id_map)?;
        for parent_ref in &proposal.parent_entity_refs {
            let parent = node_ref(parent_ref, &local_entities, viewer_id)
                .ok_or_else(|| StoreError::Repo(format!("unresolvable node ref: {parent_ref}")))?;
            store.upsert_edge(
                &source,
                "SUBTYPE_OF",
                &parent,
                &serde_json::json!({"viewer_id": viewer_id}),
                "ai_semantic",
                Some(proposal.confidence),
                &evidence,
                run_id,
                None,
            )?;
        }
    }

    for relation in &output.relations {
        let subject =
            node_ref(&relation.subject_ref, &local_entities, viewer_id).ok_or_else(|| {
                StoreError::Repo(format!("unresolvable node ref: {}", relation.subject_ref))
            })?;
        let object =
            node_ref(&relation.object_ref, &local_entities, viewer_id).ok_or_else(|| {
                StoreError::Repo(format!("unresolvable node ref: {}", relation.object_ref))
            })?;
        let predicate = if relation.predicate.is_empty() {
            "RELATED_TO"
        } else {
            relation.predicate.as_str()
        };
        let evidence = resolve_evidence(&relation.evidence_mention_ids, &mention_id_map)?;
        store.upsert_edge(
            &subject,
            predicate,
            &object,
            &serde_json::json!({
                "viewer_id": viewer_id,
                "interpretation": relation.interpretation,
            }),
            "ai_semantic",
            Some(relation.confidence),
            &evidence,
            run_id,
            None,
        )?;
    }
    store.close_missing_viewer_semantic_edges(viewer_id, run_id)?;

    apply_interest_states(
        store,
        run_id,
        viewer_id,
        output,
        &local_entities,
        &mention_id_map,
    )
}

/// §8.2 INTERESTED_IN 幂等：签名 = (target, canonical properties)。
/// - 同 target + 同签名 → 活跃边查重-合并（保留 edge_id/valid_from）；
/// - 同 target + 签名变化 → 关闭旧区间，开新区间；
/// - 旧活跃边不在新提交目标集 → 关闭区间。
fn apply_interest_states(
    store: &Store,
    run_id: &str,
    viewer_id: &str,
    output: &ViewerPerceptionSubmission,
    local_entities: &std::collections::HashMap<String, String>,
    mention_id_map: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let viewer_node = format!("viewer:{viewer_id}");
    let existing = store.active_edges(&viewer_node, "INTERESTED_IN", "ai_state")?;
    let mut kept_targets: Vec<String> = Vec::new();
    for state in &output.interest_states {
        let target = node_ref(&state.entity_ref, local_entities, viewer_id).ok_or_else(|| {
            StoreError::Repo(format!("unresolvable node ref: {}", state.entity_ref))
        })?;
        if kept_targets.contains(&target) {
            return Err(StoreError::Repo(format!(
                "duplicate interest_state target: {target}"
            )));
        }
        kept_targets.push(target.clone());
        let props = serde_json::json!({
            "status": state.status,
            "preference": state.preference,
            "aspects": state.aspects,
            "rationale": state.rationale,
        });
        let signature = json_canon(&props);
        // 内容变化：先关闭该 target 下所有签名不同的旧区间，upsert 将开新区间。
        for edge in existing
            .iter()
            .filter(|edge| edge.target_id == target && edge.properties_json != signature.as_str())
        {
            store.close_edge(&edge.edge_id, run_id, &store.now())?;
        }
        let evidence = resolve_evidence(&state.evidence_mention_ids, mention_id_map)?;
        store.upsert_edge(
            &viewer_node,
            "INTERESTED_IN",
            &target,
            &props,
            "ai_state",
            Some(state.confidence),
            &evidence,
            run_id,
            None,
        )?;
    }
    for edge in existing
        .iter()
        .filter(|edge| !kept_targets.contains(&edge.target_id))
    {
        store.close_edge(&edge.edge_id, run_id, &store.now())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 整体态势提交应用
// ---------------------------------------------------------------------------

pub fn apply_audience_submission(
    store: &Store,
    run_id: &str,
    submission: &AudienceSituationSubmission,
) -> Result<()> {
    with_savepoint("audience_apply", store, || {
        apply_audience_inner(store, run_id, submission)
    })
}

fn apply_audience_inner(
    store: &Store,
    run_id: &str,
    submission: &AudienceSituationSubmission,
) -> Result<()> {
    for (index, situation) in submission.situations.iter().enumerate() {
        let ordinal = (index + 1) as i64;
        let situation_id = format!(
            "situation:{}",
            hash_parts(
                &[
                    run_id.to_string(),
                    py_str_int(ordinal),
                    situation.title.clone()
                ],
                24,
            )
        );
        let shown = if situation.title.is_empty() {
            format!("Situation {ordinal}")
        } else {
            situation.title.clone()
        };
        store.upsert_node(
            &situation_id,
            "Situation",
            &shown,
            &serde_json::to_value(situation).map_err(|err| StoreError::Repo(err.to_string()))?,
            "ai_situation",
            None,
        )?;
        let targets: Vec<String> = situation
            .viewer_ids
            .iter()
            .map(|viewer_id| format!("viewer:{viewer_id}"))
            .chain(situation.entity_ids.iter().cloned())
            .collect();
        for target in targets {
            store.upsert_edge(
                &situation_id,
                "INVOLVES",
                &target,
                &serde_json::json!({}),
                "ai_situation",
                Some(situation.confidence),
                &situation.evidence_mention_ids,
                run_id,
                None,
            )?;
        }
    }
    for (index, action) in submission.content_opportunities.iter().enumerate() {
        let ordinal = (index + 1) as i64;
        let action_id = format!(
            "action:{}",
            hash_parts(
                &[
                    run_id.to_string(),
                    py_str_int(ordinal),
                    action.title.clone()
                ],
                24,
            )
        );
        let shown = if action.title.is_empty() {
            format!("Action {ordinal}")
        } else {
            action.title.clone()
        };
        store.upsert_node(
            &action_id,
            "Action",
            &shown,
            &serde_json::to_value(action).map_err(|err| StoreError::Repo(err.to_string()))?,
            "ai_action",
            None,
        )?;
        // v6：TARGETS/ABOUT 必带 confidence（ConfidenceWord → 数值）
        let confidence = Some(confidence_word_score(&action.confidence));
        for viewer_id in &action.audience_ids {
            store.upsert_edge(
                &action_id,
                "TARGETS",
                &format!("viewer:{viewer_id}"),
                &serde_json::json!({}),
                "ai_action",
                confidence,
                &action.evidence_mention_ids,
                run_id,
                None,
            )?;
        }
        if !action.entity_id.is_empty() {
            store.upsert_edge(
                &action_id,
                "ABOUT",
                &action.entity_id,
                &serde_json::json!({}),
                "ai_action",
                confidence,
                &action.evidence_mention_ids,
                run_id,
                None,
            )?;
        }
    }
    Ok(())
}
