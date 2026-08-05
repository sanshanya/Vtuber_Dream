//! M4 评审修复4：§9「图写入幂等、关系证据合并、旧状态失效」的断言化补钉。
//! 背景：r7-P1（upsert_edge 合并无主断言）、r7-P2（interest target 消失关臂）、
//! r7-P3（resolve_entity SAME_AS/UNCERTAIN 落库）、r5-FIND-4/5（project 臂三与
//! current_run_id=None 组合臂零覆盖）。

use std::path::Path;

use live_core::graph::build::apply_viewer_submission;
use live_core::graph::store::Store;
use live_core::models::ViewerPerceptionSubmission;
use serde_json::{Value, json};

const FIXED_NOW: &str = "2026-08-03T07:02:19.191879+00:00";

fn mem_store() -> Store {
    Store::open_with_clock(Path::new(":memory:"), || FIXED_NOW.to_string()).unwrap()
}

fn viewer_submission(
    uid: &str,
    entities: &[&str],
    interests: &[&str],
) -> ViewerPerceptionSubmission {
    let mentions: Vec<Value> = vec![];
    let entities_json: Vec<Value> = entities
        .iter()
        .enumerate()
        .map(|(i, name)| {
            json!({
                "local_id": format!("e{}", i + 1), "canonical_name": name, "entity_type": "game",
                "aliases": [], "description": "", "existing_entity_id": null,
                "resolution": "NEW_ENTITY", "evidence_mention_ids": [], "parent_entity_refs": [],
                "confidence": 0.8
            })
        })
        .collect();
    let states: Vec<Value> = interests
        .iter()
        .map(|name| {
            let index = entities.iter().position(|e| e == name).unwrap() + 1;
            json!({
                "entity_ref": format!("entity:e{index}"), "status": "近期上升",
                "preference": "关注具体内容", "aspects": [], "rationale": "r",
                "evidence_mention_ids": [], "confidence": 0.5
            })
        })
        .collect();
    serde_json::from_value(json!({
        "viewer_id": uid,
        "profile_summary": "该观众近期集中关注演示作品，优先新内容。",
        "mentions": mentions,
        "entities": entities_json,
        "relations": [],
        "interest_states": states,
        "content_preferences": [], "recent_changes": [], "hypotheses": [],
        "conversation_openers": [], "content_ideas": [], "enrichment_targets": [],
        "cautions": [], "leads": []
    }))
    .unwrap()
}

/// 单一边的活跃行计数。
fn active_count(store: &Store, predicate: &str) -> i64 {
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE predicate=? AND valid_to IS NULL",
            [predicate],
            |row| row.get(0),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// 1. r7-P1：upsert_edge 活跃边合并：evidence 并集去重保序 + confidence=max
// ---------------------------------------------------------------------------

#[test]
fn upsert_edge_merges_evidence_and_takes_max_confidence() {
    let store = mem_store();
    store
        .begin_run_fixed("run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", FIXED_NOW, "m")
        .unwrap();
    store
        .upsert_node("viewer:v", "Viewer", "v", &json!({}), "platform_fact", None)
        .unwrap();
    store
        .upsert_node("entity:game:e", "Entity", "e", &json!({}), "ai", None)
        .unwrap();
    store
        .upsert_edge(
            "viewer:v",
            "RELATED_TO",
            "entity:game:e",
            &json!({"interpretation": "一"}),
            "ai_semantic",
            Some(0.4),
            &["m1".into(), "m2".into()],
            "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("v"),
        )
        .unwrap();
    store
        .begin_run_fixed("run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", FIXED_NOW, "m")
        .unwrap();
    store
        .upsert_edge(
            "viewer:v",
            "RELATED_TO",
            "entity:game:e",
            &json!({"interpretation": "二"}),
            "ai_semantic",
            Some(0.9),
            &["m2".into(), "m3".into()],
            "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            Some("v"),
        )
        .unwrap();
    assert_eq!(
        active_count(&store, "RELATED_TO"),
        1,
        "同签名四元组不得增生边"
    );
    let (conf, evidence): (f64, String) = store
        .conn
        .query_row(
            "SELECT confidence,evidence_json FROM edges WHERE predicate='RELATED_TO'",
            [],
            |row| row.get(0).and_then(|c| row.get(1).map(|e| (c, e))),
        )
        .unwrap();
    assert_eq!(conf, 0.9, "confidence=max");
    let evidence: Vec<String> = serde_json::from_str(&evidence).unwrap();
    assert_eq!(evidence, ["m1", "m2", "m3"], "evidence 并集去重保旧序");
}

// ---------------------------------------------------------------------------
// 2. r7-P1 正臂：close_missing_viewer_semantic_edges 关闭非本运行的语义边
// ---------------------------------------------------------------------------

#[test]
fn close_missing_closes_other_runs_semantic_edges_only() {
    let store = mem_store();
    let run_a = "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let run_b = "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    store.begin_run_fixed(run_a, FIXED_NOW, "m").unwrap();
    store.begin_run_fixed(run_b, FIXED_NOW, "m").unwrap();
    store
        .upsert_node("viewer:v", "Viewer", "v", &json!({}), "platform_fact", None)
        .unwrap();
    store
        .upsert_node("entity:game:e", "Entity", "e", &json!({}), "ai", None)
        .unwrap();
    // v6 去范式：edges.viewer_id 列派生自 properties.viewer_id（非 owner 参数）。
    store
        .upsert_edge(
            "viewer:v",
            "RELATED_TO",
            "entity:game:e",
            &json!({"viewer_id": "v"}),
            "ai_semantic",
            None,
            &[],
            run_a,
            Some("v"),
        )
        .unwrap();
    // 对照组：另一观众的边 + 平台事实边不被关闭
    store
        .upsert_node("viewer:w", "Viewer", "w", &json!({}), "platform_fact", None)
        .unwrap();
    store
        .upsert_edge(
            "viewer:w",
            "RELATED_TO",
            "entity:game:e",
            &json!({"viewer_id": "w"}),
            "ai_semantic",
            None,
            &[],
            run_a,
            Some("w"),
        )
        .unwrap();
    store
        .upsert_edge(
            "viewer:v",
            "OBSERVED",
            "entity:game:e",
            &json!({}),
            "platform_fact",
            None,
            &[],
            run_a,
            None,
        )
        .unwrap();
    store
        .close_missing_viewer_semantic_edges("v", run_b)
        .unwrap();
    let closed: Vec<String> = store
        .conn
        .prepare("SELECT predicate,viewer_id FROM edges WHERE valid_to IS NOT NULL")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|r| {
            let (p, v): (String, Option<String>) = r.unwrap();
            format!("{p}/{:?}", v)
        })
        .collect();
    assert_eq!(
        closed,
        ["RELATED_TO/Some(\"v\")"],
        "只关闭 v 的其他运行语义边"
    );
}

// ---------------------------------------------------------------------------
// 3. r7-P2：interest target 从新提交消失 → 关闭区间
// ---------------------------------------------------------------------------

#[test]
fn interest_target_vanishes_closes_interval() {
    let store = mem_store();
    let run_a = "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let run_b = "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    store.begin_run_fixed(run_a, FIXED_NOW, "m").unwrap();
    store.begin_run_fixed(run_b, FIXED_NOW, "m").unwrap();
    let first = viewer_submission("v", &["异环", "明日方舟"], &["异环", "明日方舟"]);
    apply_viewer_submission(&store, run_a, "观众V", &[], &first).unwrap();
    assert_eq!(active_count(&store, "INTERESTED_IN"), 2);
    let second = viewer_submission("v", &["异环", "明日方舟"], &["异环"]);
    apply_viewer_submission(&store, run_b, "观众V", &[], &second).unwrap();
    assert_eq!(active_count(&store, "INTERESTED_IN"), 1);
    let closed: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(closed, 1, "消失的 target 必须关闭区间");
}

// ---------------------------------------------------------------------------
// 4. r7-P3：resolve_entity 落库行为：SAME_AS 建 alias，UNCERTAIN 不进正式库
// ---------------------------------------------------------------------------

#[test]
fn resolve_entity_same_as_writes_alias_and_uncertain_skips_book() {
    let store = mem_store();
    let run_a = "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    store.begin_run_fixed(run_a, FIXED_NOW, "m").unwrap();
    let first = viewer_submission("v", &["异环"], &[]);
    apply_viewer_submission(&store, run_a, "观众V", &[], &first).unwrap();
    let entity_id: String = store
        .conn
        .query_row("SELECT entity_id FROM entities LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    // SAME_AS：同名新提交 → 复用既有实体，行数不变
    let mut second = viewer_submission("w", &["异环"], &[]);
    second.entities[0].resolution = "SAME_AS".to_string();
    second.entities[0].existing_entity_id = Some(entity_id.clone());
    apply_viewer_submission(&store, run_a, "观众W", &[], &second).unwrap();
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "SAME_AS 不新建实体");
    // UNCERTAIN：不进正式实体库
    let mut uncertain = viewer_submission("x", &["异环"], &[]);
    uncertain.entities[0].resolution = "UNCERTAIN".to_string();
    apply_viewer_submission(&store, run_a, "观众X", &[], &uncertain).unwrap();
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "UNCERTAIN 不落正式库");
}

// ---------------------------------------------------------------------------
// 5. r5-FIND-4/5：project 臂三（include_situation_actions=true ∧ run_id=None）
//    与 current_run_id=None 的闸门组合
// ---------------------------------------------------------------------------

#[test]
fn project_default_arm_keeps_ai_edges_and_entire_node_set() {
    let store = mem_store();
    let run_a = "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    store.begin_run_fixed(run_a, FIXED_NOW, "m").unwrap();
    let submission = viewer_submission("v", &["异环"], &["异环"]);
    apply_viewer_submission(&store, run_a, "观众V", &[], &submission).unwrap();
    // arm 三（project 默认）：
    let options = live_core::graph::project::ProjectOptions {
        include_situation_actions: true,
        situation_run_id: None,
        current_run_id: None,
        ..Default::default()
    };
    let graph = live_core::graph::project::project(&store, &options).unwrap();
    assert!(
        graph["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["predicate"] == "INTERESTED_IN"),
        "current_run_id=None → ai_state 边不被闸门过滤"
    );
    // Python 闸门 `(? IS NULL OR ... OR run_id=?)`：None → 左臂恒真 → 不过滤。
    // 与 r5 评审裁决一致：None 组合臂的存在即闸门短路，断言反向钉死。
    let options_shadow = live_core::graph::project::ProjectOptions {
        include_situation_actions: false,
        situation_run_id: None,
        current_run_id: None,
        ..Default::default()
    };
    let shadow = live_core::graph::project::project(&store, &options_shadow).unwrap();
    assert!(
        shadow["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["predicate"] == "INTERESTED_IN"),
        "臂一 + current_run_id=None → 闸门短路，不过滤"
    );
    // 过滤器只在显式 current_run_id 下生效：别的 run 的 ai_state 边被排掉，本运行保留
    let options_other = live_core::graph::project::ProjectOptions {
        include_situation_actions: false,
        situation_run_id: None,
        current_run_id: Some("run:cccccccccccccccccccccccccccccccc".to_string()),
        ..Default::default()
    };
    let other = live_core::graph::project::project(&store, &options_other).unwrap();
    assert!(
        !other["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["predicate"] == "INTERESTED_IN"),
        "current_run_id=其他运行 → 本运行 ai_state 边被过滤"
    );
}

// ---------------------------------------------------------------------------
// 6. r4 demo 补钉：python 键序重建函数对表外键的削落必须有声
// ---------------------------------------------------------------------------

#[test]
fn demo_python_order_drops_only_unlisted_keys() {
    let submission: live_core::models::AudienceSituationSubmission =
        serde_json::from_value(json!({"executive_summary": "s"})).unwrap();
    let value = live_core::demo::python_order_audience(&submission);
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys.first(), Some(&"executive_summary"));
    assert!(!keys.contains(&"leads"), "leads 不在 Python 字面");
    // r4：表外键削落必须有声——声明内的键全量出场（防某键被无意移出投影表）。
    for key in [
        "audience_structure",
        "interest_graph",
        "communities",
        "situations",
        "content_opportunities",
        "individual_highlights",
        "content_calendar",
        "data_gaps",
        "safety_notes",
    ] {
        assert!(keys.contains(&key), "缺键 {key}");
    }
    let viewer: live_core::models::ViewerPerceptionSubmission = serde_json::from_value(json!({
        "viewer_id": "v", "profile_summary": "p"
    }))
    .unwrap();
    let viewer_keys: Vec<String> = live_core::demo::python_order_viewer(&viewer)
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert!(!viewer_keys.contains(&"leads".to_string()));
    assert_eq!(viewer_keys.len(), 13, "Python 字面 13 键");
}

// ---------------------------------------------------------------------------
// 轮2-R1-A⑤：存量 evidence_json 损坏时 upsert_edge 必须报错而非静默清空——
// 修前 `from_str(...).unwrap_or_default()` 会把旧证据坍缩为空向量并物理覆写，
// 既往证据不可逆丢失（图层的唯一事件溯源裂纹）。
// ---------------------------------------------------------------------------

#[test]
fn upsert_edge_with_corrupt_stored_evidence_errors_instead_of_wiping() {
    let store = mem_store();
    store.begin_run_fixed("run:a", FIXED_NOW, "m").unwrap();
    store
        .upsert_node("viewer:v", "Viewer", "v", &json!({}), "platform_fact", None)
        .unwrap();
    store
        .upsert_node("entity:game:e", "Entity", "e", &json!({}), "ai", None)
        .unwrap();
    let edge_id = store
        .upsert_edge(
            "viewer:v",
            "RELATED_TO",
            "entity:game:e",
            &json!({}),
            "ai_semantic",
            Some(0.5),
            &["m1".into()],
            "run:a",
            Some("v"),
        )
        .unwrap();
    // 手损：模拟迁移/手改库造成的非法 evidence_json
    store
        .conn
        .execute(
            "UPDATE edges SET evidence_json='{{{broken' WHERE edge_id=?",
            rusqlite::params![edge_id],
        )
        .unwrap();
    store.begin_run_fixed("run:b", FIXED_NOW, "m").unwrap();
    let err = store
        .upsert_edge(
            "viewer:v",
            "RELATED_TO",
            "entity:game:e",
            &json!({}),
            "ai_semantic",
            Some(0.9),
            &["m2".into()],
            "run:b",
            Some("v"),
        )
        .expect_err("损坏证据必须响亮报错，不得静默清空覆写");
    assert!(
        err.to_string().contains("evidence") || err.to_string().contains("证据"),
        "{err}"
    );
    // 原行零伤：错误路径不得半写
    let stored: String = store
        .conn
        .query_row(
            "SELECT evidence_json FROM edges WHERE edge_id=?",
            rusqlite::params![edge_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "{{{broken", "原证据必须原样无损留存");
}
