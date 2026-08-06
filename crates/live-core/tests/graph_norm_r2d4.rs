//! R2 批1「实体 AI 归并」的 store 面钉团：entity_drop（整货删除）与
//! list_entities（分页读）——程序事实/身份/事务层，AI 只提交裁决。
//!
//! 钉面：
//! - drop：entities/nodes/entity_aliases 行删净、指向它的 edges 整行删净、
//!   mention 行结构性不动（schema v8 mentions 无 entity 列）、自身不记账；
//! - drop 报错面：未知实体（NotFound）、非 'ai' source_kind（平台事实）→ Invalid
//!   且零写入；
//! - list_entities 分页：稳定排序（entity_id 字节序）、offset/limit 语义、
//!   limit 钳制 ≤100、offset 负数钳 0、aliases 列形状（NULL 分割）。

use std::path::Path;

use live_core::episodes::{Episode, EpisodeField};
use live_core::graph::build::apply_viewer_submission;
use live_core::graph::query::list_entities;
use live_core::graph::store::{MaintenanceError, Store};
use live_core::models::ViewerPerceptionSubmission;
use serde_json::{Value, json};

const FIXED_NOW: &str = "2026-08-03T07:02:19.191879+00:00";
const RUN_A: &str = "run:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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
    mentions: &[(&str, &str, i64, i64)],
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

fn count(store: &Store, sql: &str) -> i64 {
    store.conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// drop 布景：viewer v 错judged 出单实体「环世界」+ 别名环 + 两 mention；
/// 兴趣边与 REFERS_TO 边都指向该实体（drop 应整行删净）。
struct DropFixture {
    store: Store,
    entity: String,
    runs_before: i64,
    mentions_before: i64,
}

fn drop_fixture() -> DropFixture {
    let store = mem_store();
    store.begin_run_fixed(RUN_A, FIXED_NOW, "m").unwrap();
    let sub = one_entity_submission(
        "v",
        "环世界",
        &["环"],
        &[("m1", "环宝", 0, 2), ("m2", "环世界", 3, 5)],
        Some(0.5),
        0.8,
    );
    apply_viewer_submission(
        &store,
        RUN_A,
        "观众V",
        &[episode_for("v", "环宝 环世界")],
        &sub,
    )
    .unwrap();
    let entity = entity_id_of(&store, "环世界");
    // entity_drop 自身不记账：先钉 run 数快照（design：入账归 reconcile 外层维护 run）。
    let runs_before = count(&store, "SELECT COUNT(*) FROM graph_runs");
    let mentions_before = count(&store, "SELECT COUNT(*) FROM mentions WHERE viewer_id='v'");
    DropFixture {
        entity,
        store,
        runs_before,
        mentions_before,
    }
}

fn alias_count(store: &Store, entity: &str) -> i64 {
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM entity_aliases WHERE entity_id=?",
            [entity],
            |row| row.get(0),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// drop 钉①：实体/节点/别名行删净 + 引用它的边整行删 + mention 幸存 + 不记账
// ---------------------------------------------------------------------------

#[test]
fn entity_drop_removes_entity_node_aliases_and_incident_edges_only() {
    let fx = drop_fixture();
    fx.store.entity_drop(&fx.entity).unwrap();

    // 行删净：entities / nodes（Entity 镜像）/ entity_aliases。
    assert!(!fx.store.entity_exists(&fx.entity).unwrap());
    let node: Option<String> = fx
        .store
        .conn
        .query_row(
            "SELECT node_type FROM nodes WHERE node_id=?",
            [&fx.entity],
            |row| row.get(0),
        )
        .ok();
    assert!(node.is_none(), "Entity 节点镜像必须删净");
    assert_eq!(alias_count(&fx.store, &fx.entity), 0);
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM entities"), 0);

    // 引用释放：REFERS_TO（mention→entity）与 INTERESTED_IN（viewer→entity）整行删。
    let incident: i64 = fx
        .store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE source_id=? OR target_id=?",
            [&fx.entity, &fx.entity],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(incident, 0, "任何区间的边都不得残留对已删实体的引用");

    // mention 幸存：mentions 表无 entity 列（FK→episodes only），结构性不动。
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM mentions WHERE viewer_id='v'"
        ),
        fx.mentions_before,
        "mention 行必须原样幸存（无任何 entity 引用可清）"
    );
    // CONTAINS_MENTION（episode 容器边）不指向 entity → 幸存。
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM edges WHERE predicate='CONTAINS_MENTION'"
        ),
        2,
        "事实层容器边不动（锚在 episode，不在 entity）"
    );

    // entity_drop 自身不记账：run 数不变（design：入账归 reconcile 外层维护 run）。
    assert_eq!(
        count(&fx.store, "SELECT COUNT(*) FROM graph_runs"),
        fx.runs_before,
        "drop 不产生 run 行"
    );
}

// ---------------------------------------------------------------------------
// drop 钉②：platform 事实（source_kind != 'ai'）拒绝 + 未知实体 404 面
// ---------------------------------------------------------------------------

#[test]
fn entity_drop_rejects_platform_facts_and_unknown_ids() {
    let fx = drop_fixture();
    let platform_id = "entity:game:platform-demo";
    fx.store
        .upsert_platform_entity(
            platform_id,
            "平台星图",
            "game",
            &json!({"identity_source": "bilibili"}),
        )
        .unwrap();

    let err = fx.store.entity_drop(platform_id).unwrap_err();
    assert!(matches!(err, MaintenanceError::Invalid(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains("platform_fact"), "{text}");
    assert!(text.contains(platform_id), "{text}");

    let err = fx.store.entity_drop("entity:game:ghost").unwrap_err();
    assert!(matches!(err, MaintenanceError::NotFound(_)), "{err}");

    // 零写入：平台实体仍在，AI 实体未被动。
    assert!(fx.store.entity_exists(platform_id).unwrap());
    assert!(fx.store.entity_exists(&fx.entity).unwrap());
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM entities"), 2);
}

// ---------------------------------------------------------------------------
// merge 事实闸钉（外部复审如实指控的洞）：平台事实只可作 target（被并入、
// 自身行不改写），作 merge 源必拒——merge 会实删源实体，§4 红线同 drop 面。
// ---------------------------------------------------------------------------

#[test]
fn entity_merge_rejects_platform_fact_source_but_allows_target() {
    let fx = drop_fixture();
    let platform_id = "entity:game:platform-demo";
    fx.store
        .upsert_platform_entity(
            platform_id,
            "平台星图",
            "game",
            &json!({"identity_source": "bilibili"}),
        )
        .unwrap();

    // 平台事实作源：Invalid，且零写入（实体双方均在）。
    let err = fx
        .store
        .entity_merge(&[platform_id.to_string()], &fx.entity)
        .unwrap_err();
    assert!(matches!(err, MaintenanceError::Invalid(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains("platform_fact"), "{text}");
    assert!(text.contains(platform_id), "{text}");
    assert!(fx.store.entity_exists(platform_id).unwrap());
    assert!(fx.store.entity_exists(&fx.entity).unwrap());

    // 平台事实作 target：AI 碎片并入事实实体（事实行不被删除）→ 放行。
    let outcome = fx
        .store
        .entity_merge(std::slice::from_ref(&fx.entity), platform_id)
        .unwrap();
    assert!(outcome.changed);
    assert!(fx.store.entity_exists(platform_id).unwrap());
    assert!(!fx.store.entity_exists(&fx.entity).unwrap());
}

// ---------------------------------------------------------------------------
// list_entities 分页钉：稳定排序 / offset、limit 语义 / 钳制 / aliases 形状
// ---------------------------------------------------------------------------

#[test]
fn list_entities_paginates_stably_and_clamps_bounds() {
    let store = mem_store();
    store.begin_run_fixed(RUN_A, FIXED_NOW, "m").unwrap();
    for (viewer, name, aliases) in [
        ("v1", "艾尔登法环", vec!["老头环", "环"]),
        ("v2", "明日方舟", vec!["舟"]),
        ("v3", "异环", vec![]),
        ("v4", "界环", vec!["环世界"]),
    ] {
        let sub = one_entity_submission(
            viewer,
            name,
            &aliases,
            &[(&format!("m:{viewer}"), "片段", 0, 2)],
            Some(0.6),
            0.8,
        );
        apply_viewer_submission(&store, RUN_A, "观众", &[episode_for(viewer, "片段")], &sub)
            .unwrap();
    }
    let full = list_entities(&store, 0, 100).unwrap();
    assert_eq!(full["count"], 4);
    assert_eq!(full["limit"], 100);
    let items = full["items"].as_array().unwrap();
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|item| item["entity_id"].as_str())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "全页按 entity_id 稳定排序");

    // aliases 列形状：GROUP_CONCAT 重建为数组（全集 = canonical ∪ 提交别名；无别名 → []）。
    // 顺序不承重（SQLite GROUP_CONCAT 走连接扫描序）——只审集合面。
    let aliased = items
        .iter()
        .find(|item| item["canonical_name"] == "艾尔登法环")
        .unwrap();
    let mut alias_list: Vec<String> = aliased["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    alias_list.sort();
    let mut expected = vec!["艾尔登法环", "老头环", "环"];
    expected.sort();
    assert_eq!(alias_list, expected);
    // 无额外别名的实体：aliases 至少含 canonical 自身（resolve_entity 恒写 canonical 别名行）。
    let bare = items
        .iter()
        .find(|item| item["canonical_name"] == "异环")
        .unwrap();
    assert_eq!(
        bare["aliases"].as_array().unwrap().as_slice(),
        &[json!("异环")],
        "canonical 恒在别名面"
    );

    // 分页：offset/limit 语义 + 全页拼接 = offset=0 limit=100。
    let page1 = list_entities(&store, 0, 2).unwrap();
    let page2 = list_entities(&store, 2, 2).unwrap();
    assert_eq!(page1["count"], 2);
    assert_eq!(page2["count"], 2);
    assert_eq!(page1["items"][0]["entity_id"], items[0]["entity_id"]);
    assert_eq!(page1["items"][1]["entity_id"], items[1]["entity_id"]);
    assert_eq!(page2["items"][0]["entity_id"], items[2]["entity_id"]);

    // 越界 offset → 空页；limit 钳制 ≤100；负数 offset 钳 0。
    assert_eq!(list_entities(&store, 99, 10).unwrap()["count"], 0);
    assert_eq!(list_entities(&store, -5, 10).unwrap()["offset"], 0);
    assert_eq!(list_entities(&store, 0, 999).unwrap()["limit"], 100);
}
