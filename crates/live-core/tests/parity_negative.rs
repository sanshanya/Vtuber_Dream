//! 批次 0 负例 parity 测试：黄金样本的四个盲区 + 8-agent 评估发现的字节分歧。
//! 期望值均由 Python 基准实现现场计算（见 docs/2026-08-03-m1-code-eval.md §3）。

use std::path::Path;

use live_core::episodes::{
    build_viewer_episodes, deterministic_mention_seeds, evidence_to_episode, validate_span,
    viewer_evidence,
};
use live_core::graph::build::{apply_viewer_submission, ingest_room_spine};
use live_core::graph::store::Store;
use live_core::models::{
    EntityProposal, InterestStateProposal, MentionSpan, ViewerPerceptionSubmission,
};
use serde_json::{Value, json};

const FIXED_NOW: &str = "2026-08-03T07:02:19.191879+00:00";

fn mem_store() -> Store {
    Store::open_with_clock(Path::new(":memory:"), || FIXED_NOW.to_string()).unwrap()
}

fn begin(store: &Store, run_id: &str) {
    store
        .begin_run_fixed(run_id, FIXED_NOW, "custom-reasoning-model")
        .unwrap();
}

// ---------------------------------------------------------------------------
// 1. evidence_id 哈希槽位必须用原始 raw.source/raw.title（回退只用于展示）
// ---------------------------------------------------------------------------

#[test]
fn evidence_id_uses_raw_source_and_title_slots() {
    let viewer = json!({
        "viewer": {"id": "u"},
        "sources": {
            "followings": {"items": [
                // 无 source 键：展示 source 回退 "following"，但哈希槽位为 ""
                // title="" 有 creator_name：展示 title 回退 "UP"，但哈希槽位为 ""
                {"id": "i1", "creator_name": "UP", "title": "", "url": "u1"},
                // 无 source 无 title 无 creator_name
                {"id": "i2", "url": "u2"}
            ]}
        }
    });
    let evidence = viewer_evidence(&viewer, 1000);
    assert_eq!(evidence.len(), 2);
    let first = &evidence[0];
    assert_eq!(first["id"].as_str().unwrap(), "7f19e2fabdb848e6");
    assert_eq!(first["source"].as_str().unwrap(), "following"); // 展示回退
    assert_eq!(first["title"].as_str().unwrap(), "UP"); // 展示回退
    assert_eq!(first["source_label"].as_str().unwrap(), "公开关注");
    let second = &evidence[1];
    assert_eq!(second["id"].as_str().unwrap(), "54f90f64b87c03a2");
    assert_eq!(second["title"].as_str().unwrap(), "");
}

// ---------------------------------------------------------------------------
// 2. event_type 回退独立于 source 回退：source=unknown 但 event_type=public_observation
// ---------------------------------------------------------------------------

#[test]
fn event_type_falls_back_to_observation_independently() {
    let episode = evidence_to_episode("v", &json!({"id": "e"}), None);
    assert_eq!(episode.source, "unknown");
    assert_eq!(episode.event_type, "public_observation");
    assert_eq!(episode.episode_id, "episode:v:e:9aa16f1bc5fe24f6");
}

// ---------------------------------------------------------------------------
// 3. parse_time 保留小数秒（Python datetime.timestamp() 语义）
// ---------------------------------------------------------------------------

#[test]
fn evidence_sorting_preserves_subsecond_precision() {
    let viewer = json!({
        "viewer": {"id": "u"},
        "sources": {
            "favorites": {"items": [
                {"id": "a", "source": "favorite", "title": "整秒", "published_at": "2026-07-12T08:00:00+00:00"},
                {"id": "b", "source": "favorite", "title": "半秒", "published_at": "2026-07-12T08:00:00.5+00:00"}
            ]}
        }
    });
    let evidence = viewer_evidence(&viewer, 1000);
    // 半秒更新的应排第一（Python 按 1783843200.5 > 1783843200.0 排序）
    assert_eq!(evidence[0]["title"].as_str().unwrap(), "半秒");
    assert_eq!(evidence[1]["title"].as_str().unwrap(), "整秒");
}

// ---------------------------------------------------------------------------
// 4. edges.viewer_id 列口径：ai_semantic 且 properties 无 viewer_id → 列空，
//    close_missing 不得关闭它（v5 json_extract 语义等价）
// ---------------------------------------------------------------------------

#[test]
fn ai_semantic_edge_without_viewer_property_has_empty_viewer_column() {
    let store = mem_store();
    begin(&store, "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store
        .upsert_node("viewer:y", "Viewer", "y", &json!({}), "platform_fact", None)
        .unwrap();
    store
        .upsert_node("entity:game:x", "Entity", "x", &json!({}), "ai", None)
        .unwrap();
    store
        .upsert_edge(
            "viewer:y",
            "RELATED_TO",
            "entity:game:x",
            &json!({}), // 无 viewer_id property
            "ai_semantic",
            Some(0.5),
            &[],
            "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
        )
        .unwrap();
    let column: String = store
        .conn
        .query_row("SELECT viewer_id FROM edges LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(column, "", "ai_semantic 无 viewer_id property 时列必须为空");
    // v5 语义：json_extract 为 NULL 的行不匹配任何 viewer → 不被 close_missing 关闭
    store
        .close_missing_viewer_semantic_edges("y", "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        .unwrap();
    let open: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE valid_to IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        open, 1,
        "v5 等价：无 viewer 属性的 ai_semantic 边不应被关闭"
    );
}

// ---------------------------------------------------------------------------
// 5. 未知证据引用：显式报错（对齐 Python KeyError），不再静默丢弃
// ---------------------------------------------------------------------------

fn demo1_like_submission(viewer_id: &str) -> ViewerPerceptionSubmission {
    ViewerPerceptionSubmission {
        viewer_id: viewer_id.to_string(),
        profile_summary: "s".to_string(),
        mentions: vec![MentionSpan {
            mention_id: "m1".to_string(),
            episode_id: "episode:demo-1:39581170fe829af2:e431120c067efe82".to_string(),
            field_path: "title".to_string(),
            text: "异环".to_string(),
            start: 1,
            end: 3,
            mention_type: "game".to_string(),
            origin: "explicit".to_string(),
            proposed_entity_name: "异环".to_string(),
            proposed_entity_type: "game".to_string(),
            entity_ref: "entity:e1".to_string(),
            confidence: 0.96,
        }],
        entities: vec![EntityProposal {
            local_id: "e1".to_string(),
            canonical_name: "异环".to_string(),
            entity_type: "game".to_string(),
            aliases: vec![],
            description: String::new(),
            existing_entity_id: None,
            resolution: "NEW_ENTITY".to_string(),
            evidence_mention_ids: vec!["m1".to_string()],
            parent_entity_refs: vec![],
            confidence: 0.94,
        }],
        relations: vec![],
        interest_states: vec![],
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

#[test]
fn unknown_evidence_reference_is_rejected() {
    let store = mem_store();
    begin(&store, "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let viewer_json: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/viewers/demo-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let episodes = build_viewer_episodes(&viewer_json, 1000);
    let mut submission = demo1_like_submission("demo-1");
    submission.entities[0].evidence_mention_ids = vec!["missing-mention".to_string()];
    let result = apply_viewer_submission(
        &store,
        "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "演示观众A",
        &episodes,
        &submission,
    );
    assert!(result.is_err(), "未知 evidence_mention_id 必须显式失败");
}

// ---------------------------------------------------------------------------
// 6. 同一提交内重复 interest target：显式报错（守 §8.2 interval 保留承诺）
// ---------------------------------------------------------------------------

#[test]
fn duplicate_interest_target_in_one_submission_is_rejected() {
    let store = mem_store();
    begin(&store, "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let viewer_json: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/viewers/demo-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let episodes = build_viewer_episodes(&viewer_json, 1000);
    let mut submission = demo1_like_submission("demo-1");
    submission.interest_states = vec![
        InterestStateProposal {
            entity_ref: "entity:e1".to_string(),
            status: "稳定".to_string(),
            preference: String::new(),
            aspects: vec![],
            rationale: String::new(),
            evidence_mention_ids: vec!["m1".to_string()],
            confidence: 0.8,
        },
        InterestStateProposal {
            entity_ref: "entity:e1".to_string(),
            status: "近期上升".to_string(),
            preference: String::new(),
            aspects: vec![],
            rationale: String::new(),
            evidence_mention_ids: vec!["m1".to_string()],
            confidence: 0.9,
        },
    ];
    let result = apply_viewer_submission(
        &store,
        "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "演示观众A",
        &episodes,
        &submission,
    );
    assert!(
        result.is_err(),
        "重复 target 的 interest_states 必须显式失败"
    );
}

// ---------------------------------------------------------------------------
// 7. search_entities 上限 100（Python min(limit, 100)）
// ---------------------------------------------------------------------------

#[test]
fn search_entities_caps_at_python_limit() {
    let store = mem_store();
    for index in 0..120 {
        store
            .upsert_platform_entity(
                &format!("entity:game:{index:03}"),
                &format!("游戏{index:03}"),
                "game",
                &json!({}),
            )
            .unwrap();
    }
    let results = live_core::graph::query::search_entities(&store, "游戏", "game", 300).unwrap();
    assert_eq!(results.len(), 100, "上限必须对齐 Python 的 min(limit, 100)");
    let results = live_core::graph::query::search_entities(&store, "游戏", "game", 20).unwrap();
    assert_eq!(results.len(), 20);
}

// ---------------------------------------------------------------------------
// 8. resolve_entity：aliases 落表 + 0.0 confidence 显式钉住（v6 收紧，偏离 Python 的 or-0.5）
// ---------------------------------------------------------------------------

#[test]
fn resolve_entity_persists_aliases_and_exact_confidence() {
    let store = mem_store();
    let proposal = EntityProposal {
        local_id: "e1".to_string(),
        canonical_name: "异环".to_string(),
        entity_type: "game".to_string(),
        aliases: vec!["Hotta 异环".to_string(), "异环".to_string()], // 与 canonical 重复者去重
        description: String::new(),
        existing_entity_id: None,
        resolution: "NEW_ENTITY".to_string(),
        evidence_mention_ids: vec!["mention:v:abc".to_string()],
        parent_entity_refs: vec![],
        confidence: 0.0, // v6 显式收紧：忠实写 0.0（Python 的 or-0.5 容错已落档废弃）
    };
    let (resolved, decision) = store
        .resolve_entity(
            &proposal,
            "run:r",
            "viewer-x",
            &["mention:v:abc".to_string()],
        )
        .unwrap();
    assert_eq!(decision, "NEW_ENTITY");
    let alias_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM entity_aliases WHERE entity_id=?",
            [&resolved],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(alias_count, 2, "canonical + 去重后的 alias 各一行");
    let alias_conf: f64 = store
        .conn
        .query_row(
            "SELECT MAX(confidence) FROM entity_aliases WHERE entity_id=?",
            [&resolved],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        alias_conf, 0.0,
        "0.0 必须忠实写入（v6 收紧，见 eval 文档 §4）"
    );
    let props: String = store
        .conn
        .query_row(
            "SELECT properties_json FROM entities WHERE entity_id=?",
            [&resolved],
            |row| row.get(0),
        )
        .unwrap();
    assert!(props.contains("\"confidence\":0.0"));
}

// ---------------------------------------------------------------------------
// 9. room spine：GUARD_OF / OWNS_ROOM 落边 + 重跑幂等
// ---------------------------------------------------------------------------

#[test]
fn room_spine_edges_exist_and_rerun_is_idempotent() {
    let store = mem_store();
    begin(&store, "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    ingest_room_spine(
        &store,
        "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "1790370612",
        "3546595083683995",
        &["111".to_string(), "222".to_string()],
    )
    .unwrap();
    let count = |pred: &str| -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE predicate=? AND valid_to IS NULL",
                [pred],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(count("OWNS_ROOM"), 1);
    assert_eq!(count("GUARD_OF"), 2);
    let edge_ids: Vec<String> = store
        .conn
        .prepare("SELECT edge_id FROM edges ORDER BY edge_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    // 换个 run 同内容重跑：查重-合并，不产生新边
    begin(&store, "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    ingest_room_spine(
        &store,
        "run:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "1790370612",
        "3546595083683995",
        &["111".to_string(), "222".to_string()],
    )
    .unwrap();
    assert_eq!(count("OWNS_ROOM"), 1);
    assert_eq!(count("GUARD_OF"), 2);
    let edge_ids2: Vec<String> = store
        .conn
        .prepare("SELECT edge_id FROM edges ORDER BY edge_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(edge_ids, edge_ids2, "room spine 重跑必须保持边 ID 集合不变");
}

// ---------------------------------------------------------------------------
// 10. seeds 与 span（M3 消费者的 parity 钉住）
// ---------------------------------------------------------------------------

#[test]
fn mention_seeds_match_python_surface_extraction() {
    let viewer_json: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/viewers/demo-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let episodes = build_viewer_episodes(&viewer_json, 1000);
    let seeds = deterministic_mention_seeds(&episodes);
    let ids: Vec<&str> = seeds
        .iter()
        .map(|seed| seed["seed_id"].as_str().unwrap())
        .collect();
    // Python 锚点：quoted 《异环》 + platform 全长字段（tags[0]/creator_name/platform_category.name）
    assert!(
        ids.contains(&"seed:703a1af7a12accfa1ed6"),
        "quoted seed: {ids:?}"
    );
    assert!(
        ids.contains(&"seed:ffaefa74b35d75b37b46"),
        "tag0 seed: {ids:?}"
    );
    assert!(
        ids.contains(&"seed:81af34a21ecd89b85a00"),
        "creator seed: {ids:?}"
    );
    assert!(
        ids.contains(&"seed:f34163272a18401c3295"),
        "category seed: {ids:?}"
    );
    // quoted seed 的形状：字符偏移 + 文本
    let quoted = seeds
        .iter()
        .find(|seed| seed["seed_id"].as_str().unwrap() == "seed:703a1af7a12accfa1ed6")
        .unwrap();
    assert_eq!(quoted["text"].as_str().unwrap(), "异环");
    assert_eq!(quoted["start"].as_i64().unwrap(), 1);
    assert_eq!(quoted["end"].as_i64().unwrap(), 3);
    assert_eq!(
        quoted["surface_kind"].as_str().unwrap(),
        "quoted_expression"
    );
}

#[test]
fn span_validation_accepts_exact_and_rejects_mismatch() {
    let viewer_json: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/viewers/demo-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let episodes = build_viewer_episodes(&viewer_json, 1000);
    let episode = &episodes[0];
    assert_eq!(validate_span(episode, "title", "异环", 1, 3), None);
    let mismatch = validate_span(episode, "title", "异环", 0, 2).unwrap();
    assert!(mismatch.contains("span mismatch"), "{mismatch}");
    let bad_offsets = validate_span(episode, "title", "异环", 3, 3).unwrap();
    assert!(bad_offsets.contains("invalid offsets"), "{bad_offsets}");
    let no_field = validate_span(episode, "nope", "异环", 0, 2).unwrap();
    assert!(no_field.contains("has no field"), "{no_field}");
}

// ---------------------------------------------------------------------------
// 12. M3 读面基元：references 按表分流存在性（未知 id 剔除）；
//     episodes() 回读排序 + *_json 字段解析
// ---------------------------------------------------------------------------

#[test]
fn references_split_by_table_and_drop_unknown_ids() {
    let store = mem_store();
    begin(&store, "run:ffffffffffffffffffffffffffffffff");
    let viewer_json: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/viewers/demo-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let episodes = build_viewer_episodes(&viewer_json, 1000);
    apply_viewer_submission(
        &store,
        "run:ffffffffffffffffffffffffffffffff",
        "演示观众A",
        &episodes,
        &demo1_like_submission("demo-1"),
    )
    .unwrap();
    let mention_id: String = store
        .conn
        .query_row("SELECT mention_id FROM mentions", [], |row| row.get(0))
        .unwrap();
    let entity_id: String = store
        .conn
        .query_row("SELECT entity_id FROM entities LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();

    let references = live_core::graph::query::references(
        &store,
        std::slice::from_ref(&entity_id),
        &["episode:demo-1:39581170fe829af2:e431120c067efe82".to_string()],
        std::slice::from_ref(&mention_id),
    )
    .unwrap();
    assert!(references["entities"].contains(&entity_id));
    assert!(references["episodes"].contains("episode:demo-1:39581170fe829af2:e431120c067efe82"));
    assert!(references["mentions"].contains(&mention_id));

    let references = live_core::graph::query::references(
        &store,
        &["entity:game:missing".to_string()],
        &["episode:v:e:missing".to_string()],
        &["mention:v:missing".to_string()],
    )
    .unwrap();
    assert!(references["entities"].is_empty(), "未知 entity id 被剔除");
    assert!(references["episodes"].is_empty(), "未知 episode id 被剔除");
    assert!(references["mentions"].is_empty(), "未知 mention id 被剔除");
}

#[test]
fn episodes_readback_parses_json_fields_and_clamps_limit() {
    let store = mem_store();
    begin(&store, "run:ffffffffffffffffffffffffffffffff");
    let viewer_json: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/viewers/demo-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let episodes = build_viewer_episodes(&viewer_json, 1000);
    apply_viewer_submission(
        &store,
        "run:ffffffffffffffffffffffffffffffff",
        "演示观众A",
        &episodes,
        &demo1_like_submission("demo-1"),
    )
    .unwrap();

    let rows = live_core::graph::query::episodes(&store, "demo-1", None).unwrap();
    assert_eq!(rows.len(), episodes.len());
    for row in &rows {
        // Python dict(row) + pop 语义：*_json 文本列被解析并改名为 fields/platform_facts
        assert!(row.get("fields_json").is_none(), "fields_json 不应回传");
        assert!(
            row.get("fields").and_then(Value::as_array).is_some(),
            "fields 必须是解析后的数组"
        );
        assert!(
            row.get("platform_facts").is_some_and(Value::is_object),
            "platform_facts 必须是解析后的对象"
        );
    }
    let ids: std::collections::BTreeSet<&str> = rows
        .iter()
        .filter_map(|row| row.get("episode_id").and_then(Value::as_str))
        .collect();
    let expected: std::collections::BTreeSet<&str> = episodes
        .iter()
        .map(|episode| episode.episode_id.as_str())
        .collect();
    assert_eq!(ids, expected);

    let limited = live_core::graph::query::episodes(&store, "demo-1", Some(1)).unwrap();
    assert_eq!(limited.len(), 1, "limit 钳制后只回一行");
    let empty = live_core::graph::query::episodes(&store, "ghost-viewer", None).unwrap();
    assert!(empty.is_empty(), "未知 viewer 回空集");
}

/// r5-FIND-1 记录测试（已知分叉，防误改）：应用层 upsert 保证活跃 (u,v) 唯一，
/// project()/detect_communities 复刻 Python 时依赖此前提而不自行去重。
/// 手工 SQL 注入重复活跃 INTERESTED_IN 边（正常路径不可造）时：
/// - project 导出的 edges 保留两行（不做 networkx add_edge 式折叠）；
/// - detect_communities 对同一 pair 的权重累加后照常合并（Python 则只取最后权重）。
/// - detect_communities 对同一 pair 的权重累加后照常合并（Python 则只取最后权重）。
///
/// 若未来实现改为「最后权重胜出」以拉近 parity，本测试必须同步改写。
#[test]
fn duplicate_active_edges_no_folding_documented() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("g.sqlite3")).expect("store opens");
    store
        .conn
        .execute(
            "INSERT INTO graph_runs(run_id, started_at) VALUES('run-1','2026-08-01T00:00:00')",
            [],
        )
        .unwrap();
    for (id, t) in [("viewer:v1", "Viewer"), ("entity:e1", "Entity")] {
        store
            .conn
            .execute(
                "INSERT INTO nodes(node_id,node_type,name,source_kind,first_seen_at,last_seen_at) \
                 VALUES(?,?,?, 'ai_semantic','t','t')",
                rusqlite::params![id, t, id],
            )
            .unwrap();
    }
    for (edge_id, confidence) in [("edge-1", 0.9), ("edge-2", 0.1)] {
        store
            .conn
            .execute(
                "INSERT INTO edges(edge_id,source_id,predicate,target_id,source_kind,confidence,\
                 valid_from,first_seen_at,last_seen_at,run_id) \
                 VALUES(?, 'viewer:v1','INTERESTED_IN','entity:e1','ai_semantic',?,'t','t','t','run-1')",
                rusqlite::params![edge_id, confidence],
            )
            .unwrap();
    }
    let graph = live_core::graph::project::project(
        &store,
        &live_core::graph::project::ProjectOptions {
            include_episodes: false,
            include_interest_states: true,
            include_situation_actions: false,
            ..Default::default()
        },
    )
    .expect("project builds");
    let pairs: Vec<(&str, &str)> = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["predicate"] == "INTERESTED_IN")
        .map(|e| (e["source"].as_str().unwrap(), e["target"].as_str().unwrap()))
        .collect();
    assert_eq!(
        pairs,
        vec![("viewer:v1", "entity:e1"), ("viewer:v1", "entity:e1")],
        "project 导出面不折叠重复活跃边（r5-FIND-1 登记）"
    );
    // 权重累加（0.9+0.1=1.0）下仍然正常合并为单社区——分叉点在聚合数值而非崩溃。
    assert_eq!(graph["communities"].as_array().unwrap().len(), 1);
}
