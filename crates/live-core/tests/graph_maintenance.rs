//! §8.6（design 行 224-229）手动图维护钉团：entity_split / entity_merge 受控写入
//! + MAINTENANCE run 类型。钉面：
//! - split：mention 归属重指（REFERS_TO 旧闭新开的区间语义）+ 证据落在这些
//!   mention 上的关系/兴趣边关闭 + 其余边不动 + 新实体 nodes 镜像 + run 记；
//! - merge：边/别名合流（confidence=max、evidence 并集去重与 upsert 同族）、
//!   源实体关闭（行删除：entities 无 valid_to 列，§8.6 只给边定区间语义）；
//! - 两个幂等重放（同参 = 同终态，不增生 run/实体/边，重放返回原 run_id）；
//! - 显式报错面：不属于该实体的 mention、未知实体/mention、空参、source==target。

use std::path::Path;

use live_core::episodes::{Episode, EpisodeField};
use live_core::graph::build::apply_viewer_submission;
use live_core::graph::store::{MaintenanceError, MergeOutcome, SplitOutcome, Store};
use live_core::models::ViewerPerceptionSubmission;
use serde_json::{Value, json};

const FIXED_NOW: &str = "2026-08-03T07:02:19.191879+00:00";
const RUN_A: &str = "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RUN_B: &str = "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const RUN_C: &str = "run:cccccccccccccccccccccccccccccccc";

fn mem_store() -> Store {
    Store::open_with_clock(Path::new(":memory:"), || FIXED_NOW.to_string()).unwrap()
}

fn episode_for(viewer: &str, text: &str) -> Episode {
    Episode {
        episode_id: format!("episode:{viewer}:ep1"),
        viewer_id: viewer.to_string(),
        source: "bilibili".to_string(),
        event_type: "video".to_string(),
        observed_at: FIXED_NOW.to_string(),
        published_at: FIXED_NOW.to_string(),
        title: "样例".to_string(),
        url: format!("https://example.com/{viewer}"),
        bvid: "BV1xx4y1c7Ef".to_string(),
        fields: vec![EpisodeField {
            path: "title".to_string(),
            text: text.to_string(),
            kind: "title".to_string(),
        }],
        platform_facts: json!({}),
    }
}

/// 单实体提交：mentions[0..] 全部 entity_ref→entity:e1（canonical=name）。
fn one_entity_submission(
    viewer: &str,
    name: &str,
    aliases: &[&str],
    mentions: &[(&str, &str, i64, i64)], // (mid, text, start, end)
    interest_confidence: Option<f64>,
    confidence: f64,
) -> ViewerPerceptionSubmission {
    let mentions_json: Vec<Value> = mentions
        .iter()
        .map(|(mid, text, start, end)| {
            json!({
                "mention_id": mid, "episode_id": format!("episode:{viewer}:ep1"),
                "field_path": "title", "text": text, "start": start, "end": end,
                "mention_type": "作品", "origin": "explicit",
                "proposed_entity_name": name, "proposed_entity_type": "game",
                "entity_ref": "entity:e1", "confidence": confidence,
            })
        })
        .collect();
    let evidence: Vec<String> = mentions.iter().map(|(mid, ..)| mid.to_string()).collect();
    let states: Vec<Value> = match interest_confidence {
        Some(conf) => vec![json!({
            "entity_ref": "entity:e1", "status": "近期上升", "preference": "关注具体内容",
            "aspects": [], "rationale": "r", "evidence_mention_ids": evidence,
            "confidence": conf,
        })],
        None => vec![],
    };
    serde_json::from_value(json!({
        "viewer_id": viewer,
        "profile_summary": "该观众近期集中关注演示作品，优先新内容。",
        "mentions": mentions_json,
        "entities": [{
            "local_id": "e1", "canonical_name": name, "entity_type": "game",
            "aliases": aliases, "description": "d", "existing_entity_id": null,
            "resolution": "NEW_ENTITY", "evidence_mention_ids": evidence,
            "parent_entity_refs": [], "confidence": confidence,
        }],
        "relations": [],
        "interest_states": states,
        "content_preferences": [], "recent_changes": [], "hypotheses": [],
        "conversation_openers": [], "content_ideas": [], "enrichment_targets": [],
        "cautions": [], "leads": []
    }))
    .unwrap()
}

fn entity_id_of(store: &Store, canonical_name: &str) -> String {
    store
        .conn
        .query_row(
            "SELECT entity_id FROM entities WHERE canonical_name=?",
            [canonical_name],
            |row| row.get(0),
        )
        .unwrap()
}

fn mention_id_of_text(store: &Store, text: &str) -> String {
    store
        .conn
        .query_row(
            "SELECT mention_id FROM mentions WHERE text=?",
            [text],
            |row| row.get(0),
        )
        .unwrap()
}

/// mention 的活跃 REFERS_TO target —— 无活跃行 → None。
fn active_refers_to(store: &Store, mention_id: &str) -> Option<String> {
    store
        .conn
        .query_row(
            "SELECT target_id FROM edges WHERE source_id=? AND predicate='REFERS_TO' \
             AND source_kind='grounded_ai' AND valid_to IS NULL",
            [mention_id],
            |row| row.get(0),
        )
        .ok()
}

fn count(store: &Store, sql: &str) -> i64 {
    store.conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn run_kind(store: &Store, run_id: &str) -> String {
    store
        .conn
        .query_row(
            "SELECT kind FROM graph_runs WHERE run_id=?",
            [run_id],
            |row| row.get(0),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// split 布景：错并现场——viewer v 的 m1(环宝)/m2(异环) 被 AI 判成同一实体「异环」；
// viewer w 的 m3(明日方舟) 自立门户（对照组，任何维护都不得碰）。
// ---------------------------------------------------------------------------

struct SplitFixture {
    store: Store,
    entity: String,
    other_entity: String,
    m1: String,
    m2: String,
    m3: String,
}

fn split_fixture() -> SplitFixture {
    let store = mem_store();
    for run in [RUN_A, RUN_B] {
        store.begin_run_fixed(run, FIXED_NOW, "m").unwrap();
    }
    let sub_v = one_entity_submission(
        "v",
        "异环",
        &["环"],
        &[("m1", "环宝", 0, 2), ("m2", "异环", 3, 5)],
        Some(0.5),
        0.8,
    );
    apply_viewer_submission(
        &store,
        RUN_A,
        "观众V",
        &[episode_for("v", "环宝 异环")],
        &sub_v,
    )
    .unwrap();
    let sub_w = one_entity_submission(
        "w",
        "明日方舟",
        &[],
        &[("m3", "明日方舟", 0, 4)],
        Some(0.9),
        0.6,
    );
    apply_viewer_submission(
        &store,
        RUN_B,
        "观众W",
        &[episode_for("w", "明日方舟")],
        &sub_w,
    )
    .unwrap();
    let entity = entity_id_of(&store, "异环");
    let m1 = mention_id_of_text(&store, "环宝");
    // 关系边：e1 --RELATED_TO--> viewer:v，证据落在 m1（split 应关闭）。
    store
        .upsert_edge(
            &entity,
            "RELATED_TO",
            "viewer:v",
            &json!({"viewer_id": "v", "interpretation": "i"}),
            "ai_semantic",
            Some(0.6),
            std::slice::from_ref(&m1),
            RUN_A,
            None,
        )
        .unwrap();
    SplitFixture {
        m2: mention_id_of_text(&store, "异环"),
        m3: mention_id_of_text(&store, "明日方舟"),
        other_entity: entity_id_of(&store, "明日方舟"),
        store,
        entity,
        m1,
    }
}

// ---------------------------------------------------------------------------
// split 钉①：归属重指 + 关系/兴趣边关闭 + 其余不动 + 新实体镜像 + MAINTENANCE run
// ---------------------------------------------------------------------------

#[test]
fn entity_split_repoints_mentions_closes_tainted_edges_and_logs_run() {
    let fx = split_fixture();
    let outcome: SplitOutcome = fx
        .store
        .entity_split(&fx.entity, std::slice::from_ref(&fx.m1))
        .unwrap();
    assert!(outcome.changed, "首次应用必须 changed=true");
    assert_eq!(outcome.moved_mentions, 1);
    // 关系边 + e1 的 INTERESTED_IN：两条边的证据落在 m1 上 → 关闭。
    assert_eq!(outcome.closed_edges, 2);

    // 新实体：行 + nodes 镜像（query.references 的镜像断言面）。
    assert!(fx.store.entity_exists(&outcome.new_entity_id).unwrap());
    let mirror: Option<String> = fx
        .store
        .conn
        .query_row(
            "SELECT node_type FROM nodes WHERE node_id=?",
            [&outcome.new_entity_id],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(mirror.as_deref(), Some("Entity"), "新实体必须有 nodes 镜像");
    let (name, source_kind): (String, String) = fx
        .store
        .conn
        .query_row(
            "SELECT canonical_name,source_kind FROM entities WHERE entity_id=?",
            [&outcome.new_entity_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, "异环");
    assert_eq!(source_kind, "maintenance");

    // 归属重指 = 旧闭新开的区间语义（不是原地改 target）。
    assert_eq!(
        active_refers_to(&fx.store, &fx.m1).as_deref(),
        Some(outcome.new_entity_id.as_str()),
        "m1 归属重指到新实体"
    );
    let refers_rows: Vec<(String, Option<String>)> = {
        // valid_from 与 fixed 时钟齐平 → 闭区间优先排序才稳定（新旧两行同刻序不定）。
        let mut stmt = fx
            .store
            .conn
            .prepare(
                "SELECT target_id,valid_to FROM edges WHERE source_id=? AND predicate='REFERS_TO' \
                 ORDER BY CASE WHEN valid_to IS NULL THEN 1 ELSE 0 END, valid_from",
            )
            .unwrap();
        stmt.query_map([&fx.m1], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(
        refers_rows.len(),
        2,
        "m1 的 REFERS_TO = 旧闭区间 + 新活跃区间"
    );
    let (old_target, old_valid_to) = &refers_rows[0];
    assert_eq!(old_target, &fx.entity);
    assert!(old_valid_to.is_some(), "旧区间必须关闭");
    let decision: String = fx
        .store
        .conn
        .query_row(
            "SELECT properties_json FROM edges WHERE source_id=? AND predicate='REFERS_TO' \
             AND valid_to IS NULL",
            [&fx.m1],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        decision.contains("MAINTENANCE_SPLIT"),
        "新区间必须带维护决策标注：{decision}"
    );

    // 其余边不动。
    assert_eq!(
        active_refers_to(&fx.store, &fx.m2).as_deref(),
        Some(fx.entity.as_str()),
        "m2 仍属原实体"
    );
    assert_eq!(
        active_refers_to(&fx.store, &fx.m3).as_deref(),
        Some(fx.other_entity.as_str()),
        "对照组 mention 不动"
    );
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM edges WHERE predicate='CONTAINS_MENTION' AND valid_to IS NULL"
        ),
        3,
        "episode 容器边（事实层）不可动"
    );
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL"
        ),
        1,
        "对照组兴趣边保持活跃（e1 的兴趣边已闭）"
    );

    // MAINTENANCE run：kind 记 + 就地完成 + detail 可回放审计。
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM graph_runs"), 3);
    assert_eq!(
        run_kind(&fx.store, RUN_A),
        "pipeline",
        "原 run 默认 pipeline"
    );
    assert_eq!(run_kind(&fx.store, RUN_B), "pipeline");
    assert_eq!(run_kind(&fx.store, &outcome.run_id), "maintenance");
    let (completed, detail, model): (Option<String>, String, Option<String>) = fx
        .store
        .conn
        .query_row(
            "SELECT completed_at,detail_json,model FROM graph_runs WHERE run_id=?",
            [&outcome.run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(completed.is_some(), "维护 run 必须就地完成");
    assert_eq!(model.as_deref(), Some("manual"));
    assert!(detail.contains("entity_split"), "{detail}");
    assert!(detail.contains(&fx.entity), "{detail}");
    assert!(detail.contains(&fx.m1), "{detail}");
    assert!(detail.contains(&outcome.new_entity_id), "{detail}");
}

// ---------------------------------------------------------------------------
// split 钉②：幂等重放——同参 = 同终态，不增生实体/run/边，返回原 run_id
// ---------------------------------------------------------------------------

#[test]
fn entity_split_replay_is_noop_with_same_terminal_state() {
    let fx = split_fixture();
    let first = fx
        .store
        .entity_split(&fx.entity, std::slice::from_ref(&fx.m1))
        .unwrap();
    let (entities_n, runs_n, edges_n) = (
        count(&fx.store, "SELECT COUNT(*) FROM entities"),
        count(&fx.store, "SELECT COUNT(*) FROM graph_runs"),
        count(&fx.store, "SELECT COUNT(*) FROM edges"),
    );
    let second = fx
        .store
        .entity_split(&fx.entity, std::slice::from_ref(&fx.m1))
        .unwrap();
    assert!(!second.changed, "重放 changed=false");
    assert_eq!(second.new_entity_id, first.new_entity_id, "不产生二次拆分");
    assert_eq!(second.run_id, first.run_id, "重放返回原 run_id");
    assert_eq!(second.moved_mentions, 0);
    assert_eq!(second.closed_edges, 0);
    assert_eq!(
        count(&fx.store, "SELECT COUNT(*) FROM entities"),
        entities_n
    );
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM graph_runs"), runs_n);
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM edges"), edges_n);
    assert_eq!(
        active_refers_to(&fx.store, &fx.m1).as_deref(),
        Some(first.new_entity_id.as_str())
    );
}

// ---------------------------------------------------------------------------
// split 钉③：显式报错面（且零写入）
// ---------------------------------------------------------------------------

#[test]
fn entity_split_error_surface() {
    let fx = split_fixture();
    let err = fx
        .store
        .entity_split("entity:game:nope", std::slice::from_ref(&fx.m1))
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::NotFound(_)), "{err}");
    let err = fx
        .store
        .entity_split(&fx.entity, &["mention:v:nope".to_string()])
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::NotFound(_)), "{err}");
    // mention 不属于该实体 → 显式报错，错文点名 mention 与实体（§8.6 行 229）。
    let err = fx
        .store
        .entity_split(&fx.entity, std::slice::from_ref(&fx.m3))
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::Invalid(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains(&fx.m3), "{text}");
    assert!(text.contains(&fx.entity), "{text}");
    let err = fx.store.entity_split(&fx.entity, &[]).unwrap_err();
    assert!(matches!(err, MaintenanceError::Invalid(_)), "{err}");
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM entities"), 2);
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM graph_runs"), 2);
}

// ---------------------------------------------------------------------------
// merge 布景：同一 viewer v 被错判出双实体 e1(异环)+e2(环世界)。
// 注意：apply 的 interest 生命周期（§8.2 幂等切换）会关闭「不在新提交目标集」的
// 旧兴趣边——同 viewer 的两条活跃兴趣边只能绕开 apply 直写布景（upsert_edge 直写 +
// seen_at 钉合流 survivor 序：first_seen 不变 = 最老存活 → e1 的边是 survivor）。
// m2 先属 e1 后经 SAME_AS 迁 e2（留一条已闭 REFERS_TO→e1 验「闭区间也迁移」）。
// ---------------------------------------------------------------------------

struct MergeFixture {
    store: Store,
    source: String,
    target: String,
}

fn merge_fixture() -> MergeFixture {
    let store = mem_store();
    for run in [RUN_A, RUN_B, RUN_C] {
        store.begin_run_fixed(run, FIXED_NOW, "m").unwrap();
    }
    // run_a：e1「异环」（canonical 0.8 + alias 环）；兴趣边另行直写。
    let sub_a = one_entity_submission(
        "v",
        "异环",
        &["环"],
        &[("m1", "环宝", 0, 2), ("m2", "异环", 3, 5)],
        None,
        0.8,
    );
    apply_viewer_submission(
        &store,
        RUN_A,
        "观众V",
        &[episode_for("v", "环宝 异环")],
        &sub_a,
    )
    .unwrap();
    // run_b：e2「环世界」（canonical 0.6 + alias 异环——与 e1 的 canonical 冲突）。
    let sub_b = one_entity_submission(
        "v",
        "环世界",
        &["异环"],
        &[("m4", "环世界", 11, 14)],
        None,
        0.6,
    );
    apply_viewer_submission(
        &store,
        RUN_B,
        "观众V",
        &[episode_for("v", "环宝 异环")],
        &sub_b,
    )
    .unwrap();
    let source = entity_id_of(&store, "异环");
    let target = entity_id_of(&store, "环世界");
    let m1 = mention_id_of_text(&store, "环宝");
    let m2 = mention_id_of_text(&store, "异环");
    let m4 = mention_id_of_text(&store, "环世界");
    let interest_props = || json!({"status": "近期上升", "preference": "关注具体内容", "aspects": [], "rationale": "r"});
    // 两条同 quad 活跃兴趣边（seen_at 钉 survivor 序：e1 最老）。
    store
        .upsert_edge(
            "viewer:v",
            "INTERESTED_IN",
            &source,
            &interest_props(),
            "ai_state",
            Some(0.5),
            &[m1.clone(), m2.clone()],
            RUN_A,
            Some("2026-08-01T00:00:00+00:00"),
        )
        .unwrap();
    store
        .upsert_edge(
            "viewer:v",
            "INTERESTED_IN",
            &target,
            &interest_props(),
            "ai_state",
            Some(0.7),
            std::slice::from_ref(&m4),
            RUN_B,
            Some("2026-08-02T00:00:00+00:00"),
        )
        .unwrap();
    // run_c：m2 重归 e2 → m2→e1 闭区间、m2→e2 活跃。
    let span = live_core::models::MentionSpan {
        mention_id: "m2".to_string(),
        episode_id: "episode:v:ep1".to_string(),
        field_path: "title".to_string(),
        text: "异环".to_string(),
        start: 3,
        end: 5,
        mention_type: "作品".to_string(),
        origin: "explicit".to_string(),
        proposed_entity_name: "异环".to_string(),
        proposed_entity_type: "game".to_string(),
        entity_ref: String::new(),
        confidence: 0.8,
    };
    store
        .upsert_mention(&span, "v", RUN_C, Some(&target), "SAME_AS")
        .unwrap();
    MergeFixture {
        store,
        source,
        target,
    }
}

// ---------------------------------------------------------------------------
// merge 钉④：边/别名合流 + 源实体关闭 + MAINTENANCE run
// ---------------------------------------------------------------------------

#[test]
fn entity_merge_folds_edges_and_aliases_and_logs_run() {
    let fx = merge_fixture();
    let outcome: MergeOutcome = fx
        .store
        .entity_merge(std::slice::from_ref(&fx.source), &fx.target)
        .unwrap();
    assert!(outcome.changed, "首次应用必须 changed=true");
    assert_eq!(
        outcome.repointed_edges, 3,
        "迁移边 = m1 活跃 REFERS_TO + 兴趣 survivor + m2 已闭 REFERS_TO"
    );
    assert_eq!(outcome.folded_edges, 1, "同 quad 活跃兴趣边合流被吸收 1 条");
    assert_eq!(
        outcome.merged_aliases, 2,
        "源实体的 canonical + alias 环迁至目标"
    );

    // 源实体关闭：行删除 + nodes 镜像删除；全库任何区间都不得再引用它（FK 口径）。
    assert!(!fx.store.entity_exists(&fx.source).unwrap());
    assert!(fx.store.entity_exists(&fx.target).unwrap());
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM nodes WHERE node_type='Entity'"
        ),
        1,
        "Entity 节点只剩目标"
    );
    let dangling: i64 = fx
        .store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE source_id=? OR target_id=?",
            [&fx.source, &fx.source],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dangling, 0, "任何区间的边都不得引用已关闭实体");

    // 兴趣边合流：同 (viewer,predicate,target,kind) 只剩一条活跃；
    // evidence = survivor ∪ absorbed（并集去重保序）；confidence = max。
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL"
        ),
        1,
        "同 quad 活跃兴趣边必须合流为一条"
    );
    let (confidence, evidence): (f64, String) = fx
        .store
        .conn
        .query_row(
            "SELECT confidence,evidence_json FROM edges \
             WHERE predicate='INTERESTED_IN' AND valid_to IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(confidence, 0.7, "confidence=max（同 upsert 族语义）");
    let evidence: Vec<String> = serde_json::from_str(&evidence).unwrap();
    assert_eq!(
        evidence,
        [
            mention_id_of_text(&fx.store, "环宝"),
            mention_id_of_text(&fx.store, "异环"),
            mention_id_of_text(&fx.store, "环世界"),
        ],
        "evidence = survivor 序 ∪ absorbed 序"
    );
    // 合流区间：survivor 保留自己的 valid_from（first_seen 不变的最老存活口径）。
    let valid_from: String = fx
        .store
        .conn
        .query_row(
            "SELECT valid_from FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(valid_from, "2026-08-01T00:00:00+00:00");

    // 别名合流：canonical 异环 与目标原有 alias 异环 同 key → confidence=max；
    // alias 环 新增；目标自身 canonical 环世界 不动。
    let mut stmt = fx
        .store
        .conn
        .prepare(
            "SELECT alias_key,confidence FROM entity_aliases WHERE entity_id=? ORDER BY alias_key",
        )
        .unwrap();
    let aliases: Vec<(String, f64)> = stmt
        .query_map([&fx.target], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        aliases,
        vec![
            ("异环".to_string(), 0.8),
            ("环".to_string(), 0.8),
            ("环世界".to_string(), 0.6),
        ],
        "别名合流：key 冲突 confidence=max，未冲突新增（alias_key 排序为字节序）"
    );

    // MAINTENANCE run。
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM graph_runs"), 4);
    assert_eq!(run_kind(&fx.store, &outcome.run_id), "maintenance");
    let detail: String = fx
        .store
        .conn
        .query_row(
            "SELECT detail_json FROM graph_runs WHERE run_id=?",
            [&outcome.run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(detail.contains("entity_merge"), "{detail}");
    assert!(detail.contains(&fx.source), "{detail}");
    assert!(detail.contains(&fx.target), "{detail}");
}

// ---------------------------------------------------------------------------
// merge 钉⑤：幂等重放——源已关闭后同参重调 = 同终态（changed=false）
// ---------------------------------------------------------------------------

#[test]
fn entity_merge_replay_is_noop_with_same_terminal_state() {
    let fx = merge_fixture();
    let first = fx
        .store
        .entity_merge(std::slice::from_ref(&fx.source), &fx.target)
        .unwrap();
    let (edges_n, runs_n, aliases_n) = (
        count(&fx.store, "SELECT COUNT(*) FROM edges"),
        count(&fx.store, "SELECT COUNT(*) FROM graph_runs"),
        count(&fx.store, "SELECT COUNT(*) FROM entity_aliases"),
    );
    let second = fx
        .store
        .entity_merge(std::slice::from_ref(&fx.source), &fx.target)
        .unwrap();
    assert!(!second.changed, "重放 changed=false");
    assert_eq!(second.run_id, first.run_id, "重放返回原 run_id");
    assert_eq!(second.repointed_edges, 0);
    assert_eq!(second.folded_edges, 0);
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM edges"), edges_n);
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM graph_runs"), runs_n);
    assert_eq!(
        count(&fx.store, "SELECT COUNT(*) FROM entity_aliases"),
        aliases_n
    );
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL"
        ),
        1,
        "重放不得二次合流"
    );
}

// ---------------------------------------------------------------------------
// merge 钉⑤+：2026-08-13 生产 FK 爆雷回归钉——survivor 为 target 侧边时，被合
// 流的源边若只关区间不改坐标，DELETE nodes 会被 edges 的 FK 当场拒止（修复：
// 吸收边收尸连带坐标改终值）。布景 = merge_fixture 的时序倒置：源边更晚（必然
// 落 absorbed 位）。
// ---------------------------------------------------------------------------

#[test]
fn entity_merge_absorbed_source_edge_leaves_no_dangling_fk() {
    let store = mem_store();
    for run in [RUN_A, RUN_B, RUN_C] {
        store.begin_run_fixed(run, FIXED_NOW, "m").unwrap();
    }
    // e2「环世界」更老（RUN_A，seen 08-01 → survivor 位）；e1「异环」更晚（RUN_B）。
    let sub_b = one_entity_submission(
        "v",
        "环世界",
        &["异环靶"],
        &[("m4", "环世界", 11, 14)],
        None,
        0.6,
    );
    apply_viewer_submission(
        &store,
        RUN_A,
        "观众V",
        &[episode_for("v", "环宝 异环")],
        &sub_b,
    )
    .unwrap();
    let sub_a = one_entity_submission(
        "v",
        "异环",
        &["环"],
        &[("m1", "环宝", 0, 2), ("m2", "异环", 3, 5)],
        None,
        0.8,
    );
    apply_viewer_submission(
        &store,
        RUN_B,
        "观众V",
        &[episode_for("v", "环宝 异环")],
        &sub_a,
    )
    .unwrap();
    let source = entity_id_of(&store, "异环");
    let target = entity_id_of(&store, "环世界");
    let props = || json!({"status": "近期上升", "preference": "关注具体内容", "aspects": [], "rationale": "r"});
    store
        .upsert_edge(
            "viewer:v",
            "INTERESTED_IN",
            &target,
            &props(),
            "ai_state",
            Some(0.6),
            &["m4".to_string()],
            RUN_A,
            Some("2026-08-01T00:00:00+00:00"),
        )
        .unwrap();
    store
        .upsert_edge(
            "viewer:v",
            "INTERESTED_IN",
            &source,
            &props(),
            "ai_state",
            Some(0.5),
            &["m2".to_string()],
            RUN_B,
            Some("2026-08-02T00:00:00+00:00"),
        )
        .unwrap();
    // 修复前此处爆「FOREIGN KEY constraint failed」（生产实录）；修复后顺畅合流。
    let outcome = store
        .entity_merge(std::slice::from_ref(&source), &target)
        .unwrap();
    assert!(outcome.changed);
    assert_eq!(outcome.folded_edges, 1, "源的复制边必须被合流");
    // 被合流的源边：闭区间 + 坐标已改终值（不得残留 source 引用 = FK 爆雷位）。
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM edges WHERE source_id='{source}' OR target_id='{source}'"
            ),
        ),
        0,
        "任何边不得再引用源节点（FK 爆雷正是在此触发）"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL"
        ),
        1,
        "合流后活跃兴趣边只剩 survivor 一条"
    );
}

// ---------------------------------------------------------------------------
// merge 钉⑥：显式报错面（且零写入）
// ---------------------------------------------------------------------------

#[test]
fn entity_merge_error_surface() {
    let fx = merge_fixture();
    // 从未存在过的源 → NotFound（与「已合并重放」靠 MAINTENANCE 账分辨）。
    let err = fx
        .store
        .entity_merge(&["entity:game:ghost".to_string()], &fx.target)
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::NotFound(_)), "{err}");
    let err = fx
        .store
        .entity_merge(std::slice::from_ref(&fx.source), "entity:game:ghost")
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::NotFound(_)), "{err}");
    // source ∪ target 重叠 → Invalid。
    let err = fx
        .store
        .entity_merge(std::slice::from_ref(&fx.target), &fx.target)
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::Invalid(_)), "{err}");
    let err = fx
        .store
        .entity_merge(&[fx.source.clone(), fx.target.clone()], &fx.target)
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::Invalid(_)), "{err}");
    let err = fx.store.entity_merge(&[], &fx.target).unwrap_err();
    assert!(matches!(err, MaintenanceError::Invalid(_)), "{err}");
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM entities"), 2);
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM graph_runs"), 3);
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL"
        ),
        2,
        "报错面不得触碰边"
    );
}
