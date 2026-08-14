//! 「实体 AI 归并」管道尾门集成钉（用户裁决：AI 裁决归并，程序出纳）。
//! 剧本 = m4a 双观众绿跑（g1《异环》/ g2《明日方舟》），minted>0 触发 reconcile
//! 尾门：AI 提交 merge（两块作品名碎片归并到 ent_g1）+ drop（一个 source_kind='ai'
//! 的噪音外壳）。全计划由 mock tool call 给出，程序侧做存在/唯一/类型校验。
//! 断言：usage 不计 reconcile、server1 恰 4 请求、删除面按程序语义落账、
//! 维护 run 记账、REFERS_TO 重指。run2（resume 新 server）因碎片被确定性地重新
//! 铸造（first_seen 落在 run2）→ 尾门再出于独立 server2 恰再请求 1 次。
mod common;

use std::path::Path;

use serde_json::{Value, json};
use wiremock::MockServer;

use common::{assistant_tool_call, messages_len, mount_turn};
use live_core::agent::pipeline::{PipelineKnobs, run_pipeline, viewer_input_bundle};
use live_core::config::{
    AgentRuntimeConfig, AiConfig, BilibiliConfig, CollectionConfig, Config, PeerDiscoveryConfig,
    PerceptionConfig, ReasoningConfig,
};
use live_core::episodes::baseline::build_factual_baseline;
use live_core::episodes::{hash_parts, py_repr_list, safe_type};
use live_core::graph::store::{Store, mention_id_of};
use live_core::models::MentionSpan;

const SEED_ENTITY: &str = "ent-demo";
/// fixtures 标题内实测 span。
const G1_MENTION: (i64, i64, &str) = (1, 3, "异环");
const G2_MENTION: (i64, i64, &str) = (12, 16, "明日方舟");

fn m4a_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-fixtures/m4a/viewer_root")
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn test_config(root: &Path, uri: &str, resume: bool) -> Config {
    Config {
        source: root.join("config.yaml"),
        project_name: "m4c".into(),
        output_dir: root.to_path_buf(),
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
            base_url: uri.to_string(),
            api_key: "test".into(),
            model: "m4c-model".into(),
            timeout_seconds: 5.0,
            max_output_tokens: 4096,
            reasoning: ReasoningConfig {
                enabled: false,
                effort: "high".into(),
                replay_content: true,
                replay_window: None,
            },
            agent: AgentRuntimeConfig {
                resume,
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

fn episode_ids(root: &Path) -> (String, String) {
    let analysis = build_factual_baseline(root, 1000).unwrap();
    let reasoning = json!({"enabled": false, "effort": "high", "replay_content": true});
    let id_of = |uid: &str| {
        let raw = read(&root.join(format!("viewers/{uid}.json")));
        let profile = analysis["viewer_profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["viewer"]["id"] == uid)
            .unwrap()
            .clone();
        viewer_input_bundle(
            &raw,
            &profile,
            "m4c-model",
            "chat_completions",
            &reasoning,
            &["取向优先新内容与互动攻略".to_string()],
            1000,
        )
        .episodes[0]
            .episode_id
            .clone()
    };
    (id_of("g1"), id_of("g2"))
}

fn viewer_submission(
    uid: &str,
    episode_id: &str,
    mention: (i64, i64, &str),
    entity_name: &str,
) -> Value {
    json!({
        "viewer_id": uid,
        "profile_summary": "该观众近期集中关注演示作品与开放世界玩法，优先新内容和互动攻略。",
        "mentions": [{
            "mention_id": "m1", "episode_id": episode_id, "field_path": "title",
            "text": mention.2, "start": mention.0, "end": mention.1,
            "mention_type": "作品名", "origin": "explicit",
            "proposed_entity_name": entity_name, "proposed_entity_type": "游戏",
            "entity_ref": "entity:e1", "confidence": 0.9
        }],
        "entities": [{
            "local_id": "e1", "canonical_name": entity_name, "entity_type": "游戏",
            "aliases": [], "description": "", "existing_entity_id": null,
            "resolution": "NEW_ENTITY", "evidence_mention_ids": ["m1"],
            "parent_entity_refs": [], "confidence": 0.8
        }],
        "relations": [],
        "interest_states": [{
            "entity_ref": "entity:e1", "status": "近期上升", "preference": "关注具体内容",
            "aspects": [], "rationale": "公开收藏出现该实体，形成可追溯证据链。",
            "evidence_mention_ids": ["m1"], "confidence": 0.5
        }],
        "content_preferences": [], "recent_changes": [], "hypotheses": [],
        "conversation_openers": [], "content_ideas": [], "enrichment_targets": [],
        "cautions": [], "leads": []
    })
}

fn span_of(episode_id: &str, mention: (i64, i64, &str)) -> MentionSpan {
    MentionSpan {
        mention_id: "m1".to_string(),
        episode_id: episode_id.to_string(),
        field_path: "title".to_string(),
        text: mention.2.to_string(),
        start: mention.0,
        end: mention.1,
        mention_type: "作品名".to_string(),
        origin: "explicit".to_string(),
        proposed_entity_name: String::new(),
        proposed_entity_type: String::new(),
        entity_ref: "entity:e1".to_string(),
        confidence: 0.9,
    }
}

fn mention_ids() -> (String, String) {
    let (e1, e2) = episode_ids(&m4a_root());
    (
        mention_id_of("g1", &span_of(&e1, G1_MENTION)),
        mention_id_of("g2", &span_of(&e2, G2_MENTION)),
    )
}

fn audience_submission(viewers: &[&str]) -> Value {
    let (m1, m2) = mention_ids();
    let evidence: Vec<Value> = viewers
        .iter()
        .map(|v| {
            if *v == "g1" {
                json!(m1.clone())
            } else {
                json!(m2.clone())
            }
        })
        .collect();
    json!({
        "executive_summary": "观众分别围绕演示作品形成独立兴趣焦点，结构简单清晰可追溯。",
        "audience_structure": [],
        "interest_graph": [{
            "entity_id": SEED_ENTITY, "entity": "演示聚合实体", "entity_type": "游戏",
            "parent_entities": [], "angles": [], "viewer_ids": viewers,
            "status": "无法判断", "confidence": 0.6, "evidence_summary": "",
            "evidence_mention_ids": evidence
        }],
        "communities": [], "situations": [], "content_opportunities": [],
        "individual_highlights": [], "content_calendar": [],
        "data_gaps": [], "safety_notes": [], "leads": []
    })
}

fn seed_entity(store: &Store) {
    store.conn.execute(
        "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
         VALUES('ent-demo','演示聚合实体','演示聚合实体','游戏','','ai_semantic','{}','2000-01-01T00:00:00+00:00','2000-01-01T00:00:01+00:00')",
        [],
    ).unwrap();
    store
        .upsert_node(
            SEED_ENTITY,
            "Entity",
            "演示聚合实体",
            &json!({}),
            "ai_semantic",
            None,
        )
        .unwrap();
}

/// 播种一个 source_kind='ai' 的噪音实体（可被 AI drop；platform_fact 等不可删）。
fn seed_noise(store: &Store) {
    store.conn.execute(
        "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
         VALUES('ent-noise','纯噪音外壳','纯噪音外壳','游戏','','ai','{}','2000-01-01T00:00:00+00:00','2000-01-01T00:00:01+00:00')",
        [],
    ).unwrap();
}

fn name_gate(name: &'static str) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    move |body: &Value| {
        messages_len(2)(body)
            && body["messages"][1]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("对下面完整Episode") && s.contains(name))
    }
}

fn audience_gate() -> impl Fn(&Value) -> bool + Send + Sync + 'static + Clone {
    |body: &Value| {
        messages_len(2)(body)
            && body["messages"][1]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("基于下面的全员索引"))
    }
}

/// reconcile 尾门首回合：system instructions + user prompt（出战提示语含注册表总数）。
fn reconcile_gate() -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    |body: &Value| {
        messages_len(2)(body)
            && body["messages"][1]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("当前长期实体注册表共"))
    }
}

async fn mount_viewer_ok(server: &MockServer, name: &'static str, submission: Value) {
    mount_turn(
        server,
        name_gate(name),
        assistant_tool_call(
            "call-v1",
            "submit_viewer_perception",
            json!({"submission": submission}),
            None,
        ),
    )
    .await;
}

async fn mount_audience_ok(server: &MockServer, viewers: &[&str]) {
    mount_turn(
        server,
        audience_gate(),
        assistant_tool_call(
            "call-a1",
            "submit_audience_situation",
            json!({"submission": audience_submission(viewers)}),
            None,
        ),
    )
    .await;
}

async fn mount_reconcile_ok(server: &MockServer, id: &str, draft: Value) {
    mount_turn(
        server,
        reconcile_gate(),
        assistant_tool_call(
            id,
            "submit_entity_reconcile",
            json!({"submission": draft}),
            None,
        ),
    )
    .await;
}

/// 决定论复算观众提案的碎片实体 id（resolve_entity 公式的镜像，同公式可漂即错）。
fn fragment_entity_id(viewer: &str, entity_type: &str, mention_id: &str) -> String {
    format!(
        "entity:{}:{}",
        safe_type(entity_type),
        hash_parts(
            &[
                viewer.to_string(),
                entity_type.to_string(),
                py_repr_list(&[mention_id.to_string()]),
                String::new(),
            ],
            18,
        )
    )
}

async fn setup_root() -> (tempfile::TempDir, Value) {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&m4a_root(), tmp.path());
    let analysis = build_factual_baseline(tmp.path(), 1000).unwrap();
    (tmp, analysis)
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn open_store(root: &Path) -> Store {
    Store::open(&root.join("graph/perception.sqlite3")).unwrap()
}

// ---------------------------------------------------------------------------
// 1. 自动带（删码刀12）：同键严格碰撞零 LLM 直并；无候选 → 裁决带整带跳过
// ---------------------------------------------------------------------------

/// 布景：两个 canonical 全等的 ai 碎片（碰撞组；零边零别名 → degree 并列，
/// 字节序小者 ent-auto-a 按目标排序为正主）。
fn seed_collision_pair(store: &Store) {
    for id in ["ent-auto-a", "ent-auto-b"] {
        store.conn.execute(
            "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
             VALUES(?1,'分身壳甲','分身壳甲','游戏','','ai','{}','2000-01-01T00:00:00+00:00','2000-01-01T00:00:01+00:00')",
            [id],
        ).unwrap();
    }
}

async fn mount_green_run(server: &MockServer, tmp: &Path) {
    let (e1, e2) = episode_ids(tmp);
    mount_viewer_ok(
        server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_viewer_ok(
        server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    mount_audience_ok(server, &["g1", "g2"]).await;
}

fn entity_exists(store: &Store, id: &str) -> bool {
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE entity_id=?",
            [id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

fn maintenance_run_count(store: &Store) -> i64 {
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM graph_runs WHERE kind='maintenance'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_reconcile_auto_band_merges_strict_collision_without_llm() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    mount_green_run(&server, tmp.path()).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    seed_noise(&store);
    seed_collision_pair(&store);
    drop(store);

    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("green run with auto band");
    assert_eq!(result["status"], "complete");
    assert_eq!(result["usage"]["llm_requests"], 3, "usage 不含 reconcile");
    // 观众×2 + audience×1 = 3：碰撞对被自动带直并；碎片名互不相似 → 候选空 →
    // 裁决带整带零调用（成本官规：无活不烧 LLM）。
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "自动带直并零 LLM"
    );

    let store = open_store(tmp.path());
    assert!(entity_exists(&store, "ent-auto-a"), "正主存活");
    assert!(
        !entity_exists(&store, "ent-auto-b"),
        "碰撞碎片已被自动带吸收"
    );
    // 两碎片均撞上基线平台道同名实体（异环/明日方舟皆有平台 tag——跨型严格
    // 碰撞恰是自动带最高价猎物）：碎片各被吸收进平台正主，全表同名实体各存
    // 一件，活跃 REFERS_TO 边均重指。
    let (m1, m2) = mention_ids();
    let survivor_of = |name: &str| -> (String, String) {
        let rows: Vec<(String, String)> = {
            let mut stmt = store
                .conn
                .prepare("SELECT entity_id,source_kind FROM entities WHERE canonical_name=?")
                .unwrap();
            stmt.query_map([name], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(rows.len(), 1, "{name} 全表只存正主一件：{rows:?}");
        rows.into_iter().next().unwrap()
    };
    for (mention_id, fragment, name) in [
        (&m1, fragment_entity_id("g1", "游戏", &m1), "异环"),
        (&m2, fragment_entity_id("g2", "游戏", &m2), "明日方舟"),
    ] {
        let (survivor, kind) = survivor_of(name);
        assert_ne!(survivor, fragment, "{name} 的碎片应被吸收");
        assert_eq!(kind, "platform_fact", "{name} 正主 = 平台道实体");
        assert!(!entity_exists(&store, &fragment));
        let refers_to: String = store
            .conn
            .query_row(
                "SELECT target_id FROM edges WHERE source_id=? AND predicate='REFERS_TO' \
                 AND source_kind='grounded_ai' AND valid_to IS NULL",
                [mention_id],
                |row| row.get(0),
            )
            .expect("mention 的活跃 REFERS_TO 边应存在");
        assert_eq!(refers_to, survivor, "{name} 的 mention 边应重指正主");
    }
    // 无关实体一概不扰：聚合种子、噪音外壳原样（自动带不发 drop）。
    assert!(entity_exists(&store, SEED_ENTITY));
    assert!(entity_exists(&store, "ent-noise"));
    assert!(maintenance_run_count(&store) >= 1, "自动带照旧记账");
    drop(store);
}

// ---------------------------------------------------------------------------
// 2. resume 收敛：碎片缓存重铸 → 尾门再开（零 LLM）→ 自动带状态级幂等收敛
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_resume_auto_band_is_silent_and_idempotent() {
    let server1 = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    mount_green_run(&server1, tmp.path()).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    seed_collision_pair(&store);
    drop(store);
    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server1.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run1");
    let runs_after_r1 = {
        let store = open_store(tmp.path());
        let n = maintenance_run_count(&store);
        drop(store);
        n
    };

    let server2 = MockServer::start().await;
    let mut knobs2 = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server2.uri(), true),
        &analysis,
        false,
        &mut knobs2,
    )
    .await
    .expect("run2 resumed");
    assert_eq!(result["status"], "complete");
    assert_eq!(
        server2.received_requests().await.unwrap().len(),
        0,
        "run2 全缓存 → 零 LLM 请求（尾门再开也是自动带主场）"
    );
    let (m1, m2) = mention_ids();
    let store = open_store(tmp.path());
    assert!(!entity_exists(&store, "ent-auto-b"), "自动带结果持久");
    // 自动带每次 minted>0 即收敛（=幂等于状态而非记账轴）：run1 收敛过的
    // 碎片在 run2 重铸后再次被自动带吸收，终态收敛一致——钉的是状态幂等，
    // 而非 maintenance run 轴的免重（重铸后的 merge 是真工作，不是重放）。
    assert!(maintenance_run_count(&store) >= runs_after_r1);
    for (viewer, m, name) in [("g1", &m1, "异环"), ("g2", &m2, "明日方舟")] {
        assert!(!entity_exists(
            &store,
            &fragment_entity_id(viewer, "游戏", m)
        ),);
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM entities WHERE canonical_name=? AND source_kind='platform_fact'",
                    [name],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1,
            "{name} 正主恒存"
        );
    }
    drop(store);
}

// ---------------------------------------------------------------------------
// 3. 裁决带（删码刀12）：相似非同名 → 候选清单 → LLM 判 merge/drop 落账
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_reconcile_judge_band_with_candidates_calls_llm_once() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, e2) = episode_ids(tmp.path());
    // 相似非同名（jaccard≈0.73 稳过闸）——不成碰撞、必进候选带。
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "周年装扮主题"),
    )
    .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "周年装扮主题活动"),
    )
    .await;
    mount_audience_ok(&server, &["g1", "g2"]).await;
    let store = open_store(tmp.path());
    // ent-demo 是 audience 聚合语提交的兴趣图承载位（缺了 audience 校验拒→
    // 二回合，单侧 mock 燃尽即 404——本钉翻车实录）。
    seed_entity(&store);
    seed_noise(&store);
    drop(store);

    let (m1, m2) = mention_ids();
    let ent_g1 = fragment_entity_id("g1", "游戏", &m1);
    let ent_g2 = fragment_entity_id("g2", "游戏", &m2);
    mount_reconcile_ok(
        &server,
        "call-r1",
        json!({
            "merges": [{
                "target_entity_id": ent_g1,
                "source_entity_ids": [ent_g2],
                "rationale": "同一周年装扮主题活动的分身两壳，证据链独立可追溯。",
            }],
            "drops": [{"entity_id": "ent-noise", "rationale": "无任何事实承载的纯展示噪音。"}]
        }),
    )
    .await;

    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("green run with judge band");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        4,
        "v1/v2/audience/reconcile 各一次（候选非空 → 裁决带恰一发）"
    );
    let store = open_store(tmp.path());
    assert!(entity_exists(&store, &ent_g1));
    assert!(!entity_exists(&store, &ent_g2), "裁决 merge 已落");
    assert!(!entity_exists(&store, "ent-noise"), "裁决 drop 已落");
    assert!(maintenance_run_count(&store) >= 1, "裁决带记账");
    drop(store);
}
