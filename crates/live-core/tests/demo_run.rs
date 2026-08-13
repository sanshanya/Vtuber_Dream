//! M4-D demo 集成钉：产物面、SAME_AS 共享实体、两次跑确定性（并发序 = 单线程同步通道，
//! 确定性等价于「任何并发编排下产物只依赖合成输入」）。
//!
//! 对账基座：tests-fixtures/m4d/python_demo_root（Python `live-audience demo` 实产，剥
//! graph_run_id/generated_at；G-9 验收 = 与该 fixture 全等，Python 是预言机——禁止手写期望）。
use std::path::{Path, PathBuf};

use serde_json::Value;

use live_core::config::{
    AgentRuntimeConfig, AiConfig, BilibiliConfig, CollectionConfig, Config, PeerDiscoveryConfig,
    PerceptionConfig, ReasoningConfig,
};
use live_core::demo::build_demo;
use live_core::graph::project::{self, ProjectOptions};
use live_core::graph::store::Store;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-fixtures/m4d/python_demo_root")
}

fn test_config(root: &Path) -> Config {
    Config {
        source: root.join("config.yaml"),
        project_name: "m4d".into(),
        output_dir: root.join("runs"),
        bilibili: BilibiliConfig {
            room_id: "1".into(),
            streamer_uid: "0".into(),
            cookie: "SESSDATA=test".into(),
            additional_viewer_ids: vec![],
        },
        collection: CollectionConfig {
            max_guards: 1,
            per_viewer_request_budget: 1,
            followings_limit: 1,
            recent_videos: 1,
            recent_dynamics: 1,
            favorite_folders: 1,
            favorite_items_per_folder: 1,
            bangumi_limit: 1,
            games_limit: 1,
            max_video_metadata_items: 1,
            request_delay_seconds: 0.0,
            timeout_seconds: 5.0,
            room_comment_request_budget: 0,
            live_replay_danmaku_limit: 1,
            lead_fetch_budget_per_run: 0,
        },
        perception: PerceptionConfig {
            max_evidence_per_viewer: 1000,
            preserve_raw_snapshots: false,
            platform_hot_search_limit: 1,
            minimum_community_size: 1,
            graph_default_expanded_kinds: live_core::config::GRAPH_DEFAULT_EXPANDED_KINDS
                .iter()
                .map(|kind| kind.to_string())
                .collect(),
            graph_row_limit: live_core::config::DEFAULT_GRAPH_ROW_LIMIT,
            peer: PeerDiscoveryConfig {
                candidate_limit: 1,
                recent_videos: 1,
                recent_dynamics: 1,
                max_formal_peers: 1,
            },
        },
        ai: AiConfig {
            api: "chat_completions".into(),
            base_url: "http://127.0.0.1:9".into(),
            api_key: "test".into(),
            model: "m4d-model".into(),
            timeout_seconds: 5.0,
            max_output_tokens: 4096,
            reasoning: ReasoningConfig {
                enabled: false,
                effort: "high".into(),
                replay_content: true,
                replay_window: None,
            },
            agent: AgentRuntimeConfig {
                resume: false,
                local_trace: false,
                run_retries: 0,
                retry_backoff_seconds: 0.0,
                viewer_token_budget: 200_000,
                max_parallel_viewers: 4,
                max_llm_rpm: 0,
                fold_trigger_tokens: 0,
                fold_keep_tail_turns: 2,
                fold_entry_chars: 480,
            },
            search_results_per_query: 20,
            rules: vec!["取向优先新内容与互动攻略".into()],
            run_budget_cny: None,
        },
        report_title: "t".into(),
        admin_token: String::new(),
    }
}

fn run_demo(tag: &str) -> (PathBuf, Value) {
    let root = std::env::temp_dir().join(format!("m4d-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = test_config(&root);
    let result = build_demo(&config, None).expect("build_demo");
    let demo_root = root.join("_demo");
    assert_eq!(
        result["output_dir"].as_str().unwrap(),
        demo_root.to_string_lossy()
    );
    (demo_root, result)
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}")))
        .unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

fn demo_project(demo_root: &Path) -> Value {
    let store = Store::open(&demo_root.join("graph").join("perception.sqlite3")).unwrap();
    project::project(&store, &ProjectOptions::default()).unwrap()
}

/// 对账豁免（两侧对称归一；kickoff 豁免清单 + 实测书面偏差）：
/// - 运行身份与时间面：run_id/各 created_at 系/`state_id`/由 (now,run_id) 派生的边 id
///   （`predicate` 对象连 `id` 一起剥；节点 id 是确定性内容 ID，不剥）；
/// - `situation:`/`action:` 前缀的字符串值：Python 用 run_id 加盐 → 掩码；
/// - `schema_version`：v6 去范式 vs Python v5（书面偏差）；
/// - `ai_action` 边的 `confidence`：v6 决策「TARGETS/ABOUT 必带数值」（Python 为 None）；
/// - mentions：`ORDER BY created_at DESC` 的毫秒并列次序属运行面 → 按 mention_id 排序。
fn normalize_project(value: &mut Value) {
    const VOLATILE: [&str; 8] = [
        "generated_at",
        "created_at",
        "first_seen_at",
        "last_seen_at",
        "valid_from",
        "run_id",
        "state_id",
        "schema_version",
    ];
    match value {
        Value::Object(map) => {
            for key in VOLATILE {
                map.remove(key);
            }
            if map.contains_key("predicate") {
                map.remove("id");
            }
            if map.get("source_kind").and_then(Value::as_str) == Some("ai_action") {
                map.remove("confidence");
            }
            for child in map.values_mut() {
                if let Value::String(text) = child
                    && (text.starts_with("situation:") || text.starts_with("action:"))
                {
                    *text = "*".to_string();
                } else {
                    normalize_project(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_project),
        _ => {}
    }
}

/// mentions 毫秒并列次序归一：按 mention_id 排序（两侧对称）。
fn sort_mentions(graph: &mut Value) {
    if let Some(mentions) = graph.get_mut("mentions").and_then(Value::as_array_mut) {
        mentions.sort_by(|a, b| a["mention_id"].as_str().cmp(&b["mention_id"].as_str()));
    }
}

#[test]
fn demo_artifact_surface_and_no_peers() {
    let (demo_root, result) = run_demo("surface");
    // 对账集 + state（Python demo.py 落盘面平移）
    for rel in [
        "collection.json",
        "streamer.json",
        "shared/platform_snapshot.json",
        "viewers/demo-1.json",
        "viewers/demo-2.json",
        "viewers/demo-3.json",
        "ai/perception/viewers/demo-1.json",
        "ai/perception/viewers/demo-2.json",
        "ai/perception/viewers/demo-3.json",
        "ai/situation.json",
        "ai/state.json",
        "graph/perception.sqlite3",
    ] {
        assert!(demo_root.join(rel).exists(), "缺产物 {rel}");
    }
    // 排除面
    assert!(!demo_root.join("peers").exists(), "peers 链属 D-5 书面裁剪");
    assert!(!demo_root.join("ai").join("peer_streamers.json").exists());
    assert!(!demo_root.join("site").exists(), "HTML 站挂 M5");
    // 返回 dict = Python build_demo 裁剪版（无 report 键）
    let keys: Vec<&str> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["status", "synthetic_demo", "output_dir", "graph"]);
    assert!(
        result["graph"]["database"]
            .as_str()
            .unwrap()
            .ends_with("perception.sqlite3")
    );
}

#[test]
fn demo_shared_yihuan_single_entity() {
    let (demo_root, _) = run_demo("same-as");
    let graph = demo_project(&demo_root);
    // 平台事实环还会从 tags 建 bilibili_tag:异环（platform_fact 来源）——同名两节点属正确性；
    // SAME_AS 钉 = game 型 异环 恰一（与 Python fixture 一致）。
    let entities: Vec<&Value> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| {
            n["type"].as_str() == Some("Entity")
                && n["name"].as_str() == Some("异环")
                && n["properties"]["entity_type"].as_str() == Some("game")
        })
        .collect();
    assert_eq!(entities.len(), 1, "SAME_AS 必须先映射共享实体");
    let yihuan = entities[0]["id"].as_str().unwrap();
    // demo-1/demo-2 各一条 INTERESTED_IN → 同一实体
    let lovers: Vec<&Value> = graph["interest_states"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["entity_id"].as_str() == Some(yihuan))
        .collect();
    assert_eq!(lovers.len(), 2);
    let viewers: std::collections::BTreeSet<&str> = lovers
        .iter()
        .filter_map(|s| s["viewer_id"].as_str())
        .collect();
    assert_eq!(viewers, ["demo-1", "demo-2"].into_iter().collect());
    // 两名观众各贡献一条 text=异环 mention
    let mentions: Vec<&Value> = graph["mentions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["text"].as_str() == Some("异环"))
        .collect();
    assert_eq!(mentions.len(), 2);
    // ai/state.json：complete + 三观众 hash + graph_run_id 回指
    let state = read(&demo_root.join("ai").join("state.json"));
    assert_eq!(state["status"], "complete");
    assert_eq!(state["model"], "synthetic-demo");
    assert_eq!(state["protocol"], "tool_call_only");
    assert_eq!(state["viewer_input_hashes"].as_object().unwrap().len(), 3);
    assert!(state["graph_run_id"].as_str().unwrap().starts_with("run:"));
}

#[test]
fn demo_rerun_deterministic() {
    let (root_a, _) = run_demo("twice-a");
    let (root_b, _) = run_demo("twice-b");
    let mut graph_a = demo_project(&root_a);
    let mut graph_b = demo_project(&root_b);
    for graph in [&mut graph_a, &mut graph_b] {
        normalize_project(graph);
        sort_mentions(graph);
    }
    assert_eq!(
        graph_a, graph_b,
        "两次跑的图投影必须全等（剥运行身份与时间面）"
    );
    for rel in [
        "collection.json",
        "viewers/demo-1.json",
        "ai/perception/viewers/demo-1.json",
        "ai/situation.json",
    ] {
        assert_eq!(
            read(&root_a.join(rel)),
            read(&root_b.join(rel)),
            "{rel} 两次跑不等"
        );
    }
    // graph_run_id 是运行 UUID：剥除后 state.json 也须相等
    let mut state_a = read(&root_a.join("ai").join("state.json"));
    let mut state_b = read(&root_b.join("ai").join("state.json"));
    state_a.as_object_mut().unwrap().remove("graph_run_id");
    state_b.as_object_mut().unwrap().remove("graph_run_id");
    assert_eq!(state_a, state_b);
}

#[test]
fn demo_matches_python_oracle() {
    let fixture = fixture_root();
    assert!(
        fixture.join("collection.json").exists(),
        "先生成 tests-fixtures/m4d（私仓 target/gen_m4d_golden.py）"
    );
    let (demo_root, _) = run_demo("oracle");
    // 输入面字节级对账的文件群（对账集）
    for rel in [
        "collection.json",
        "streamer.json",
        "shared/platform_snapshot.json",
        "viewers/demo-1.json",
        "viewers/demo-2.json",
        "viewers/demo-3.json",
        "ai/perception/viewers/demo-1.json",
        "ai/perception/viewers/demo-2.json",
        "ai/perception/viewers/demo-3.json",
        "ai/situation.json",
    ] {
        assert_eq!(
            read(&demo_root.join(rel)),
            read(&fixture.join(rel)),
            "与 Python demo 对账失败: {rel}"
        );
    }
    // state.json：graph_run_id 属运行身份 → 剥；Python 端 fixture 亦已剥。
    let mut mine = read(&demo_root.join("ai").join("state.json"));
    mine.as_object_mut().unwrap().remove("graph_run_id");
    assert_eq!(
        mine,
        read(&fixture.join("ai").join("state.json")),
        "ai/state.json 对账失败"
    );
    // 图投影整体对账（fixture = Python project() 实产）；两侧同一豁免面归一。
    let mut mine_graph = demo_project(&demo_root);
    let mut oracle_graph = read(&fixture.join("graph_project.json"));
    for graph in [&mut mine_graph, &mut oracle_graph] {
        normalize_project(graph);
        sort_mentions(graph);
    }
    // 调试缝：M4D_DUMP=<path> 时落盘归一后投影（对账排查用；正常跑无副作用）。
    if let Ok(dump) = std::env::var("M4D_DUMP") {
        std::fs::write(dump, serde_json::to_string_pretty(&mine_graph).unwrap()).unwrap();
    }
    assert_eq!(
        mine_graph, oracle_graph,
        "图投影与 Python demo project() 不全等"
    );
}
