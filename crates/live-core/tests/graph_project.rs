//! M4-B golden 对账：project() 与 detect_communities() vs Python/networkx 真值。
//! fixtures = tests-fixtures/m4b/（私仓 target/gen_m4b_golden.py 实算产出）。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use live_core::graph::project::{ProjectOptions, detect_communities, project};
use live_core::graph::store::Store;

fn m4b() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-fixtures/m4b")
}

fn demo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-fixtures/demo")
}

fn load(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is JSON")
}

/// graph_dump.json 七表原样灌库（行序 = fixture 数组序 → rowid/并列排序与 Python 灌库一致）。
fn store_from_dump(dir: &Path) -> Store {
    let dump = load(&demo().join("graph_dump.json"));
    let store = Store::open(&dir.join("perception.sqlite3")).expect("store opens");
    for table in [
        "graph_runs",
        "nodes",
        "edges",
        "episodes",
        "mentions",
        "entities",
        "entity_aliases",
    ] {
        let Some(rows) = dump[table].as_array() else {
            continue;
        };
        for row in rows {
            let map = row.as_object().expect("dump row is object");
            let columns: Vec<&str> = map.keys().map(String::as_str).collect();
            let sql = format!(
                "INSERT INTO {table}({}) VALUES({})",
                columns.join(","),
                columns.iter().map(|_| "?").collect::<Vec<_>>().join(",")
            );
            let values: Vec<rusqlite::types::Value> = map
                .values()
                .map(|v| match v {
                    Value::Null => rusqlite::types::Value::Null,
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            rusqlite::types::Value::Integer(i)
                        } else {
                            rusqlite::types::Value::Real(n.as_f64().unwrap())
                        }
                    }
                    Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                    other => rusqlite::types::Value::Text(other.to_string()),
                })
                .collect();
            store
                .conn
                .execute(&sql, rusqlite::params_from_iter(values.iter()))
                .unwrap_or_else(|err| panic!("insert {table}: {err}; row={row}"));
        }
    }
    store
}

// ---------------------------------------------------------------------------
// project()：demo 库 pipeline 实参全量对账
// ---------------------------------------------------------------------------

#[test]
fn project_demo_matches_python_golden() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_from_dump(tmp.path());
    let args = load(&m4b().join("demo_project_args.json"));
    let options = ProjectOptions {
        include_episodes: false,
        include_interest_states: true,
        include_situation_actions: false,
        current_run_id: args["current_run_id"].as_str().map(str::to_string),
        limit: args["limit"].as_i64(),
        minimum_community_size: args["minimum_community_size"].as_i64().unwrap(),
        ..ProjectOptions::default()
    };
    let mut built = project(&store, &options).expect("project builds");
    let mut expected = load(&m4b().join("demo_project.json"));
    expected
        .as_object_mut()
        .unwrap()
        .remove("stats_clock_field");
    // 豁免清单（kickoff 风险 ②）：时钟字段；schema_version 是并行 bump
    //（Python 库 v5 / Rust GRAPH_SCHEMA_VERSION 属版本面，非导出行为差异）——
    // 钉 Rust 恒发当前 schema 版（读面与库内常量合轴，不刻字面）。
    built["generated_at"] = Value::Null;
    expected["generated_at"] = Value::Null;
    assert_eq!(
        built["schema_version"],
        Value::from(live_core::graph::GRAPH_SCHEMA_VERSION)
    );
    expected["schema_version"] = Value::Null;
    built["schema_version"] = Value::Null;
    // 分节对比 + 首节内首个差异项定位（全量 diff 输出不可读）。
    for section in [
        "stats",
        "nodes",
        "edges",
        "mentions",
        "communities",
        "interest_states",
    ] {
        if built[section] != expected[section] {
            let left = &built[section];
            let right = &expected[section];
            let (mut idx, mut note) = (0usize, String::new());
            if let (Some(a), Some(b)) = (left.as_array(), right.as_array()) {
                for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                    if x != y {
                        let keys: Vec<String> = x
                            .as_object()
                            .map(|m| {
                                m.keys()
                                    .filter(|k| x[k.as_str()] != y[k.as_str()])
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default();
                        idx = i;
                        note = format!(
                            " keys={keys:?}
 built[{i}]={x}
 expect[{i}]={y}"
                        );
                        break;
                    }
                }
                if note.is_empty() {
                    note = format!("len: {} vs {}", a.len(), b.len());
                }
            }
            panic!("section {section} diverged at {idx}:{note}");
        }
    }
}

// ---------------------------------------------------------------------------
// detect_communities()：7 个合成案 partition 对账（含平局裁决）
// ---------------------------------------------------------------------------

#[test]
fn detect_communities_matches_networkx_golden() {
    for case in load(&m4b().join("communities_cases.json"))
        .as_array()
        .unwrap()
    {
        let built = detect_communities(
            case["nodes"].as_array().unwrap(),
            case["edges"].as_array().unwrap(),
            case["minimum_size"].as_i64().unwrap(),
        );
        assert_eq!(
            built,
            *case["expected"].as_array().unwrap(),
            "case {}",
            case["name"].as_str().unwrap()
        );
    }
}

// ---------------------------------------------------------------------------
// project() 三臂：手造 Situation/Action 双运行库（demo golden 只盖臂一）
// ---------------------------------------------------------------------------

#[test]
fn project_three_arms_situation_visibility() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("g.sqlite3")).unwrap();
    // edges.run_id → graph_runs FK：两个运行先播种。
    store.begin_run_fixed("run-cur", "t0", "test").unwrap();
    store.begin_run_fixed("run-old", "t0", "test").unwrap();
    let node = |id: &str, kind: &str, source_kind: &str| {
        store
            .conn
            .execute(
                "INSERT INTO nodes(node_id,node_type,name,properties_json,source_kind,first_seen_at,last_seen_at) \
                 VALUES(?1,?2,?3,'{}',?4,'t0','t1')",
                rusqlite::params![id, kind, id, source_kind],
            )
            .unwrap();
    };
    let edge = |id: &str, src: &str, pred: &str, tgt: &str, kind: &str, run: &str| {
        store
            .conn
            .execute(
                "INSERT INTO edges(edge_id,source_id,predicate,target_id,properties_json,source_kind,confidence,evidence_json,valid_from,valid_to,first_seen_at,last_seen_at,run_id) \
                 VALUES(?1,?2,?3,?4,'{}',?5,0.9,'[]','t0',NULL,'t0','t1',?6)",
                rusqlite::params![id, src, pred, tgt, kind, run],
            )
            .unwrap();
    };
    node("viewer:v1", "Viewer", "platform_fact");
    node("ent-1", "Entity", "ai_semantic");
    node("sit-old", "Situation", "ai_semantic");
    node("act-cur", "Action", "ai_semantic");
    edge(
        "e-state",
        "viewer:v1",
        "INTERESTED_IN",
        "ent-1",
        "ai_state",
        "run-cur",
    );
    edge(
        "e-old-sit",
        "sit-old",
        "COVERS",
        "viewer:v1",
        "ai_semantic",
        "run-old",
    );
    edge(
        "e-cur-act",
        "act-cur",
        "TARGETS",
        "ent-1",
        "ai_semantic",
        "run-cur",
    );

    let nodes_of = |built: &Value| -> Vec<String> {
        built["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect()
    };
    let edges_of = |built: &Value| -> Vec<String> {
        built["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    };

    // 臂一：include_situation_actions=false → Situation/Action 节点与其边全隐身，
    // 但当前运行的 ai_state 边可见。
    let built = project(
        &store,
        &ProjectOptions {
            include_situation_actions: false,
            current_run_id: Some("run-cur".to_string()),
            ..ProjectOptions::default()
        },
    )
    .unwrap();
    assert_eq!(nodes_of(&built), ["ent-1", "viewer:v1"]);
    assert_eq!(edges_of(&built), ["e-state"]);

    // 臂二：situation_run_id=run-old → 仅该运行有活跃出边的 Situation 节点回现；
    // act-cur 的边属 run-cur ≠ run-old → 节点不回现；其边也被 NOT EXISTS(… AND e.run_id<>run-old) 排除。
    let built = project(
        &store,
        &ProjectOptions {
            include_situation_actions: true,
            situation_run_id: Some("run-old".to_string()),
            current_run_id: Some("run-cur".to_string()),
            ..ProjectOptions::default()
        },
    )
    .unwrap();
    assert_eq!(nodes_of(&built), ["ent-1", "sit-old", "viewer:v1"]);
    assert_eq!(edges_of(&built), ["e-state"]);
}

// ---------------------------------------------------------------------------
// community_id「先编号后过滤」跳号钉——networkx enumerate 在 continue
// 之前发号；若误改为过滤后连续重排，本针必须红。
// ---------------------------------------------------------------------------

#[test]
fn communities_prefilter_numbering_gap_pinned() {
    let node = |id: &str, t: &str| serde_json::json!({"id": id, "type": t, "name": id});
    let interest = |u: &str, v: &str| {
        serde_json::json!({
            "source": u, "target": v, "predicate": "INTERESTED_IN", "confidence": 0.9
        })
    };
    // 社区 A：viewer:v1 + 4 entity（尺寸 5，viewers=1 < 2）→ 编号 community:1 后被滤。
    // 社区 B：viewer:v2/v3 + entity:e9（尺寸 3，viewers=2）→ 以原编号 community:2 留存。
    let nodes = vec![
        node("viewer:v1", "Viewer"),
        node("entity:e1", "Entity"),
        node("entity:e2", "Entity"),
        node("entity:e3", "Entity"),
        node("entity:e4", "Entity"),
        node("viewer:v2", "Viewer"),
        node("viewer:v3", "Viewer"),
        node("entity:e9", "Entity"),
    ];
    let edges = vec![
        interest("viewer:v1", "entity:e1"),
        interest("viewer:v1", "entity:e2"),
        interest("viewer:v1", "entity:e3"),
        interest("viewer:v1", "entity:e4"),
        interest("viewer:v2", "entity:e9"),
        interest("viewer:v3", "entity:e9"),
    ];
    let communities = detect_communities(&nodes, &edges, 2);
    assert_eq!(communities.len(), 1, "{communities:?}");
    assert_eq!(
        communities[0]["community_id"], "community:2",
        "community:1 被 minimum_size 滤掉后编号缺口必须保留：{communities:?}"
    );
    assert_eq!(
        communities[0]["viewer_ids"],
        serde_json::json!(["v2", "v3"])
    );
    assert_eq!(
        communities[0]["entity_ids"],
        serde_json::json!(["entity:e9"])
    );
    assert_eq!(communities[0]["member_count"], 3);
}

// ---------------------------------------------------------------------------
// LIMIT 实际截断四类产出 + stats 联动 + 下钳 max(1) 钉
// ---------------------------------------------------------------------------

#[test]
fn project_limit_truncates_all_sections_and_clamps_to_one() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_from_dump(tmp.path());
    let args = load(&m4b().join("demo_project_args.json"));
    let options = ProjectOptions {
        include_episodes: false,
        include_interest_states: true,
        include_situation_actions: false,
        current_run_id: args["current_run_id"].as_str().map(str::to_string),
        limit: Some(2),
        minimum_community_size: args["minimum_community_size"].as_i64().unwrap(),
        ..ProjectOptions::default()
    };
    let graph = project(&store, &options).expect("project builds");
    for section in ["nodes", "edges", "mentions", "interest_states"] {
        assert_eq!(
            graph[section].as_array().map(Vec::len),
            Some(2),
            "{section} 应被 limit=2 实际截断（联动 detect_communities 仅见截断输入）"
        );
    }
    // stats.nodes/edges 取截断后列表长度（与库内真实计数分离的设计面）。
    assert_eq!(graph["stats"]["nodes"], 2);
    assert_eq!(graph["stats"]["edges"], 2);

    // 下钳 max(1)：limit=0/负值不设下限会吞出全表，必须钳到 1。
    let zero = ProjectOptions {
        limit: Some(0),
        ..options
    };
    let graph = project(&store, &zero).expect("project builds");
    assert_eq!(graph["nodes"].as_array().map(Vec::len), Some(1));
    assert_eq!(graph["edges"].as_array().map(Vec::len), Some(1));
    assert_eq!(graph["mentions"].as_array().map(Vec::len), Some(1));
}
