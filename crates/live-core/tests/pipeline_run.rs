//! M4-C pipeline 集成钉：阶段状态机、缓存恢复、并发扇出+有序栅栏、失败分支、state.json。
//! 剧本基座 = tests-fixtures/m4a/viewer_root（两观众）；LLM 面由 wiremock 逐回合钉。
mod common;

use std::path::Path;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{BodyPred, assistant_bill, assistant_tool_call, messages_len, mount_turn};
use live_core::agent::pipeline::{PipelineKnobs, run_pipeline, viewer_input_bundle};
use live_core::config::{
    AgentRuntimeConfig, AiConfig, BilibiliConfig, CollectionConfig, Config, PeerDiscoveryConfig,
    PerceptionConfig, ReasoningConfig,
};
use live_core::episodes::baseline::build_factual_baseline;
use live_core::graph::store::{Store, StoreError, mention_id_of};
use live_core::models::MentionSpan;

const SEED_ENTITY: &str = "ent-demo";
/// fixtures 标题内实测 span（字符偏移；g1=《异环》…角色演出，g2=…与《明日方舟》世界观讨论）。
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
            leads_autonomy: 0,
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
                max_turns: 4,
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
        },
        report_title: "t".into(),
    }
}

/// 实测两观众的 favorite episode id（bundle episodes[0] = 收藏条目；排序钉在 M4-A bundle 对账）。
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

/// audience 提交：cite 播种实体 + 实际入库 mention（M4-A 实测 id）。
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
         VALUES('ent-demo','演示聚合实体','演示聚合实体','游戏','','ai_semantic','{}','2000-01-01T00:00:00+00:00','2000-01-01T00:01:00+00:00')",
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

fn name_gate(name: &'static str) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    move |body: &Value| {
        messages_len(2)(body)
            && body["messages"][1]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("对下面完整Episode") && s.contains(name))
    }
}

fn audience_gate() -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    move |body: &Value| {
        messages_len(2)(body)
            && body["messages"][1]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("基于下面的全员索引"))
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
// 1. 绿路全景 + 栅栏序 + 终态 parity
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_green_path_and_fence_order() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, e2) = episode_ids(tmp.path());
    // g1 慢响应（乱完成序），g2 快速——栅栏仍按 viewer_ids 序应用。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(name_gate("黄金观众甲")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(assistant_tool_call(
                    "call-v1",
                    "submit_viewer_perception",
                    json!({"submission": viewer_submission("g1", &e1, G1_MENTION, "异环")}),
                    None,
                ))
                .set_delay(std::time::Duration::from_millis(200)),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    mount_audience_ok(&server, &["g1", "g2"]).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);

    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("green run");

    // final dict parity（D-4 runtime 五键 + D-1 usage 入 state）
    assert_eq!(result["status"], "complete");
    assert_eq!(result["runtime"], "openai-agents");
    assert_eq!(result["viewer_count"], 2);
    assert_eq!(result["viewer_failures"], 0);
    assert_eq!(result["usage"]["llm_requests"], 3);
    assert_eq!(result["usage"]["input_tokens"], 30);
    // Z1/P0-2：cache 观测盒姊妹键——mock 剧本中 backend 不返 cache 计数字段 → 诚实归零；
    // 键必须存在且为整数（state 与 final_result 双面同源）。
    assert_eq!(result["cache_usage"]["cache_hit_tokens"], 0);
    assert_eq!(result["cache_usage"]["cache_miss_tokens"], 0);
    let state = read(&tmp.path().join("ai/state.json"));
    assert_eq!(state["status"], "complete");
    assert!(
        state["situation_input_hash"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "situation hash 回读落 state"
    );
    assert!(
        state["usage"]["total_tokens"].as_i64().is_some(),
        "D-1 usage 入 state"
    );
    assert!(
        state["cache_usage"]["cache_hit_tokens"].as_i64().is_some(),
        "Z1 cache_usage 入 state"
    );
    assert!(state["viewer_input_hashes"]["g1"].as_str().is_some());
    let cache = read(&tmp.path().join("ai/perception/viewers/g1.json"));
    assert_eq!(cache["status"], "complete");
    let runtime = cache["runtime"].as_object().unwrap();
    assert_eq!(runtime.len(), 5, "D-4 五键 parity，实际：{runtime:?}");
    assert!(runtime.get("tool_names").is_none());
    // r3-F3：空 leads 不落盘（Python extra=forbid——键存在即拒；空期双向复用）。
    assert!(
        cache["analysis"]
            .as_object()
            .unwrap()
            .get("leads")
            .is_none(),
        "空 leads 必须剥键"
    );
    let situation = read(&tmp.path().join("ai/situation.json"));
    assert!(
        situation["analysis"]
            .as_object()
            .unwrap()
            .get("leads")
            .is_none(),
        "audience 同剥"
    );
    // 栅栏：mentions rowid 序 = viewer_ids 序（g1 慢后完成仍先应用）
    let store = open_store(tmp.path());
    let mut stmt = store
        .conn
        .prepare("SELECT mention_id FROM mentions ORDER BY rowid")
        .unwrap();
    let order: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let (m1, m2) = mention_ids();
    assert_eq!(order, vec![m1, m2]);
    let completed_at: Option<String> = store
        .conn
        .query_row("SELECT completed_at FROM graph_runs", [], |row| row.get(0))
        .unwrap();
    assert!(completed_at.is_some(), "run 行 completed_at 落盘");
    // Z5/C1：本例 audience 提交未带 front_brief → 沉默以键缺席落盘。
    assert!(
        situation["analysis"]
            .as_object()
            .unwrap()
            .get("front_brief")
            .is_none(),
        "空 front_brief 必须剥键（缺席=沉默可呈现面）"
    );
}

// ---------------------------------------------------------------------------
// 1b. Z5/C1 front_brief：有效简报全链落地 + 无出处引用被拒后具名重提交
// ---------------------------------------------------------------------------

/// audience 提交 + 有效 front_brief（cite 真实入库 episode）：终局接受、situation 缓存留痕。
#[tokio::test(flavor = "multi_thread")]
async fn pipeline_audience_front_brief_lands_in_cache() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, _e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &_e2, G2_MENTION, "明日方舟"),
    )
    .await;
    let mut submission = audience_submission(&["g1", "g2"]);
    submission.as_object_mut().unwrap().insert(
        "front_brief".to_string(),
        json!({
            "sentences": [{
                "text": "两名舰长分别围绕《异环》和《明日方舟》形成各自的内容焦点，暂无跨人共同体信号。",
                "episode_refs": [e1],
                "coverage_time_range": ["2026-07-01", "2026-08-04"]
            }]
        }),
    );
    mount_turn(
        &server,
        audience_gate(),
        assistant_tool_call(
            "call-a1",
            "submit_audience_situation",
            json!({"submission": submission}),
            None,
        ),
    )
    .await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);

    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("green run");
    assert_eq!(result["status"], "complete");
    let situation = read(&tmp.path().join("ai/situation.json"));
    let brief = &situation["analysis"]["front_brief"];
    assert_eq!(brief["sentences"].as_array().unwrap().len(), 1, "{brief}");
    assert_eq!(
        brief["sentences"][0]["episode_refs"][0],
        json!(e1),
        "refs 原样留痕（可追溯原则）"
    );
    assert_eq!(
        brief["sentences"][0]["coverage_time_range"],
        json!(["2026-07-01", "2026-08-04"])
    );
}

/// 句句带出处是硬闸：引用不存在 episode 的简报必被终局拒收，模型修错后才接受。
#[tokio::test(flavor = "multi_thread")]
async fn pipeline_audience_front_brief_unknown_episode_rejected() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, _e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    // audience turn 1：编一个库内不存在的 episode → 终局拒收（错误具名回喂）。
    let mut bad = audience_submission(&["g1"]);
    bad.as_object_mut().unwrap().insert(
        "front_brief".to_string(),
        json!({"sentences": [{"text": "凭空的一句结论。", "episode_refs": ["episode:ghost"]}]}),
    );
    mount_turn(
        &server,
        audience_gate(),
        assistant_tool_call(
            "call-a1",
            "submit_audience_situation",
            json!({"submission": bad}),
            None,
        ),
    )
    .await;
    // audience turn 2：修正为真实入库 episode → 接受。
    let (m1, _) = mention_ids();
    mount_turn(
        &server,
        |body: &Value| messages_len(4)(body) && body["messages"][2]["tool_calls"].is_array(),
        assistant_tool_call(
            "call-a2",
            "submit_audience_situation",
            json!({"submission": {
                "executive_summary": "观众甲围绕《异环》形成单一内容焦点，证据链完整。",
                "front_brief": {"sentences": [{
                    "text": "观众甲近期收藏集中于《异环》相关内容。",
                    "episode_refs": [e1]
                }]},
                "audience_structure": [],
                "interest_graph": [{
                    "entity_id": SEED_ENTITY, "entity": "演示聚合实体", "entity_type": "游戏",
                    "parent_entities": [], "angles": [], "viewer_ids": ["g1"],
                    "status": "无法判断", "confidence": 0.6, "evidence_summary": "",
                    "evidence_mention_ids": [m1]
                }],
                "communities": [], "situations": [], "content_opportunities": [],
                "individual_highlights": [], "content_calendar": [],
                "data_gaps": [], "safety_notes": [], "leads": []
            }}),
            None,
        ),
    )
    .await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);

    // 只跑 g1：过滤 analysis 至单观众，复用 run_viewer_pipeline 语义之外的完整路径。
    let mut one = analysis.clone();
    one["viewer_profiles"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["viewer"]["id"].as_str() == Some("g1"));
    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &one,
        false,
        &mut knobs,
    )
    .await
    .expect("简报拒收后具名重提交应收敛");
    assert_eq!(result["status"], "complete");
    assert_eq!(
        result["usage"]["llm_requests"], 3,
        "g1 viewer 1 请求 + audience 拒收重交 2 请求"
    );
    let situation = read(&tmp.path().join("ai/situation.json"));
    assert_eq!(situation["status"], "complete");
    assert_eq!(
        situation["analysis"]["front_brief"]["sentences"][0]["episode_refs"][0],
        json!(e1),
        "修正后的有效 refs 原样落盘"
    );
}

// ---------------------------------------------------------------------------
// 2. 全量缓存恢复：同根重跑 → 零 LLM 调用（server 无 mock，任何请求即失败）
//    + D-10 幂等在 pipeline 层的投影（活跃边零膨胀）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_full_resume_no_llm_calls() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    mount_audience_ok(&server, &["g1", "g2"]).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run1");
    // run2：无任何 mock——成功 ⟺ 缓存全命中（重校验同样过：图已含 run1 数据）
    let mut knobs2 = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs2,
    )
    .await
    .expect("run2 must be fully cache-resumed");
    assert_eq!(result["viewer_count"], 2);
    let store = open_store(tmp.path());
    let active: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active, 2);
}

// ---------------------------------------------------------------------------
// 2b. 部分恢复端到端（r7-P4）：run1 双绿 → 删掉 g2 缓存 → run2 只重跑 g2，
//     g1 复用校准（含图 references 重校验）、audience 输入不变 → 也恢复；
//     活跃边零膨胀。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_partial_resume_reruns_only_evicted_viewer() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await; // expect(1)：run2 必须一次都不再叫 g1
    // g2 只挂一次、expect(2)：run1 一次 + run2 重跑一次。
    // （同 matcher 挂两个 expect(1) mock 时请求会全被先挂的吸收、后挂的饿死——
    //  wiremock drop 校验报 2/0，先例见 pipeline_leads 的 mount_all(times=2)。）
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(name_gate("黄金观众乙")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_tool_call(
                "call-v2",
                "submit_viewer_perception",
                json!({"submission": viewer_submission("g2", &e2, G2_MENTION, "明日方舟")}),
                None,
            )),
        )
        .expect(2)
        .mount(&server)
        .await;
    mount_audience_ok(&server, &["g1", "g2"]).await; // run1 的一次
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run1");

    // 逐出仅 g2 的缓存（模拟中断/手工清理后的部分现场）
    std::fs::remove_file(tmp.path().join("ai/perception/viewers/g2.json")).unwrap();
    let g1_cache_before =
        std::fs::metadata(tmp.path().join("ai/perception/viewers/g1.json")).unwrap();
    let g1_mtime_before = g1_cache_before.modified().unwrap();

    // g2 的第二发由上面同一个 expect(2) mock 服务；
    // audience 输入不变 → 恢复（run2 零 audience 请求，由 after-before==1 钉死）
    let before = server.received_requests().await.unwrap().len();
    let mut knobs2 = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs2,
    )
    .await
    .expect("run2 partial-resume");
    let after = server.received_requests().await.unwrap().len();
    assert_eq!(result["viewer_count"], 2);
    assert_eq!(
        after - before,
        1,
        "run2 只允许 g2 一个 LLM 请求（audience 输入不变 → 恢复）"
    );
    // g1 缓存文件未被重写（复用，非重跑再覆盖）
    let g1_mtime_after = std::fs::metadata(tmp.path().join("ai/perception/viewers/g1.json"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(g1_mtime_before, g1_mtime_after, "g1 必须走 Reused 臂");
    assert_eq!(
        read(&tmp.path().join("ai/perception/viewers/g2.json"))["status"],
        "complete"
    );
    // 图幂等：apply 双轮后 INTERESTED_IN 活跃边仍恰 2（D-10 在部分恢复下同样成立）
    let store = open_store(tmp.path());
    let active: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE predicate='INTERESTED_IN' AND valid_to IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active, 2);
}

// ---------------------------------------------------------------------------
// 3. 单观众失败继续（500 瞬时族耗尽 → 该观众 failed 缓存，另一人照常）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_single_viewer_failure_continues() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (_e1, e2) = episode_ids(tmp.path());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(name_gate("黄金观众甲")))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "server boom"}})),
        )
        .expect(5) // chat 内层瞬时重试共 5 次（2026-08-05 起 HTTP_EXTRA_ATTEMPTS=4）
        .mount(&server)
        .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    mount_audience_ok(&server, &["g2"]).await;
    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("partial run");
    assert_eq!(result["viewer_count"], 1);
    assert_eq!(result["viewer_failures"], 1);
    let cache = read(&tmp.path().join("ai/perception/viewers/g1.json"));
    assert_eq!(cache["status"], "failed");
}

// ---------------------------------------------------------------------------
// 3b. r1-F1：单 viewer token 预算熔断 → viewer_failure("token_budget")，其余照常
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_viewer_token_budget_trips_viewer_failure() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (_e1, e2) = episode_ids(tmp.path());
    // g1 第一回合即 claim 5000 tokens（预算 100）→ 熔断；不重试（expect 1）。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(name_gate("黄金观众甲")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_bill("预算烧穿的无效草稿", 5000)),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    mount_audience_ok(&server, &["g2"]).await;

    let mut config = test_config(tmp.path(), &server.uri(), true);
    config.ai.agent.viewer_token_budget = 100;
    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(config, &analysis, false, &mut knobs)
        .await
        .expect("预算熔断不得炸整轮");
    assert_eq!(result["viewer_count"], 1);
    assert_eq!(result["viewer_failures"], 1);
    let cache = read(&tmp.path().join("ai/perception/viewers/g1.json"));
    assert_eq!(cache["status"], "failed");
    assert!(
        cache["error"]
            .as_str()
            .is_some_and(|s| s.contains("token_budget")),
        "g1 缓存 error 必须含错误类别: {cache}"
    );
}

// ---------------------------------------------------------------------------
// 4. 全灭 → AgentRuntimeError parity 文案 + failed state + run 行 failed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_all_viewers_fail_aborts() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "server boom"}})),
        )
        .expect(10) // 2 viewers × 5 内层尝试（2026-08-05 起 HTTP_EXTRA_ATTEMPTS=4）
        .mount(&server)
        .await;
    let mut knobs = PipelineKnobs::default();
    let err = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect_err("must abort");
    assert_eq!(
        err.to_string(),
        "all viewer Perception or graph applies failed"
    );
    let state = read(&tmp.path().join("ai/state.json"));
    assert_eq!(state["status"], "failed");
    assert_eq!(state["viewer_stage_status"], "incomplete");
    let failed_at: Option<String> = open_store(tmp.path())
        .conn
        .query_row("SELECT failed_at FROM graph_runs", [], |row| row.get(0))
        .unwrap();
    assert!(failed_at.is_some(), "run 行 failed_at 落盘");
}

// ---------------------------------------------------------------------------
// 5. graph_failed 分支（hook 复现）+ viewer_failures 明细落 state
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_graph_failure_marks_graph_failed() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    // g2 应用失败 → audience 只见 g1 的证据
    mount_audience_ok(&server, &["g1"]).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    let mut hook = |store: &Store,
                    run_id: &str,
                    name: &str,
                    episodes: &[live_core::episodes::Episode],
                    output: &live_core::models::ViewerPerceptionSubmission| {
        if name == "黄金观众乙" {
            Err(StoreError::Repo("injected apply failure".to_string()))
        } else {
            live_core::graph::build::apply_viewer_submission(store, run_id, name, episodes, output)
        }
    };
    let mut knobs = PipelineKnobs {
        apply_viewer: Some(&mut hook),
        ..PipelineKnobs::default()
    };
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run with injected graph failure");
    assert_eq!(result["viewer_count"], 1);
    assert_eq!(result["viewer_failures"], 1);
    let cache = read(&tmp.path().join("ai/perception/viewers/g2.json"));
    assert_eq!(cache["status"], "graph_failed");
    assert!(
        cache["error"]
            .as_str()
            .is_some_and(|s| s.contains("injected"))
    );
    // Python parity：viewer_failures 明细只写在 viewer_complete 瞬态，被后续 complete 覆盖，
    // 不落终态——明细的持久面 = 每观众缓存的 graph_failed + final 计数（已钉）。
    let state = read(&tmp.path().join("ai/state.json"));
    assert_eq!(state["status"], "complete");
}

// ---------------------------------------------------------------------------
// 6. audience 失败 → 整 run 失败（state failed + viewer_stage complete + run 行 failed）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_audience_failure_fails_run() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(audience_gate()))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "server boom"}})),
        )
        .expect(5) // audience 单 chat × 5 内层尝试（HTTP_EXTRA_ATTEMPTS=4）
        .mount(&server)
        .await;
    let mut knobs = PipelineKnobs::default();
    let err = run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect_err("audience failure must fail the run");
    assert!(
        err.to_string().contains("500") || err.to_string().to_lowercase().contains("http"),
        "err={err}"
    );
    let state = read(&tmp.path().join("ai/state.json"));
    assert_eq!(state["status"], "failed");
    assert_eq!(state["viewer_stage_status"], "complete");
    let failed_at: Option<String> = open_store(tmp.path())
        .conn
        .query_row("SELECT failed_at FROM graph_runs", [], |row| row.get(0))
        .unwrap();
    assert!(failed_at.is_some(), "run 行 failed_at 落盘");
}

// ---------------------------------------------------------------------------
// 7. D-10：audience apply 幂等（同提交两次应用 → 图面零膨胀）
// ---------------------------------------------------------------------------

#[test]
fn audience_apply_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("g.sqlite3")).unwrap();
    store.begin_run_fixed("run-1", "t0", "m").unwrap();
    seed_entity(&store);
    let submission = serde_json::from_value::<live_core::models::AudienceSituationSubmission>(
        audience_submission(&["g1"]),
    )
    .unwrap();
    live_core::graph::build::apply_audience_submission(&store, "run-1", &submission).unwrap();
    let count = |sql: &str| -> i64 { store.conn.query_row(sql, [], |row| row.get(0)).unwrap() };
    let (n1, e1) = (
        count("SELECT COUNT(*) FROM nodes"),
        count("SELECT COUNT(*) FROM edges"),
    );
    live_core::graph::build::apply_audience_submission(&store, "run-1", &submission).unwrap();
    let (n2, e2) = (
        count("SELECT COUNT(*) FROM nodes"),
        count("SELECT COUNT(*) FROM edges"),
    );
    assert_eq!((n1, e1), (n2, e2));
}

// ---------------------------------------------------------------------------
// 8. 评审 M-B：恢复重校验的 search 闭包 = 子实例磁盘回填注册表（非空集）
//    剧本：绿跑 → 手工给两观众缓存的 action 挂 sr 引用 + research_cache.json 落该 sr
//    → 重跑：只挂 audience mock；viewers 全复用（Empty 闭包时代的旧行为 = 必拒必重跑）。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn resume_reuses_cache_referencing_backfilled_search_result() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    mount_audience_ok(&server, &["g1", "g2"]).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut PipelineKnobs::default(),
    )
    .await
    .expect("first green run");

    // 注入：缓存 action 引用一个「上运行发现并已归档」的 sr
    const SR: &str = "0123456789abcdef";
    for uid in ["g1", "g2"] {
        let cache_path = tmp.path().join(format!("ai/perception/viewers/{uid}.json"));
        let mut cache = read(&cache_path);
        cache["analysis"]["conversation_openers"] = json!([{
            "title": "复用检索证据的开场",
            "detail": "引用上运行归档搜索结果",
            "evidence_mention_ids": [],
            "search_result_ids": [SR],
            "observation_metrics": [],
            "risk": ""
        }]);
        std::fs::write(&cache_path, serde_json::to_string_pretty(&cache).unwrap()).unwrap();
    }
    std::fs::write(
        tmp.path().join("ai/research_cache.json"),
        serde_json::to_string_pretty(&json!({
            "searches": {"异环": [{
                "result_id": SR,
                "query": "异环",
                "title": "异环官方账号",
                "bvid": "BV1demo",
                "url": "https://www.bilibili.com/video/BV1demo"
            }]},
            "videos": {}
        }))
        .unwrap(),
    )
    .unwrap();
    // 重跑：situation.json 的 analysis 不引用 sr（仅观众侧引用）——audience 缓存也复用，
    // 零 LLM 挂载；任何请求 404 只会让 run 直接失败。
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut PipelineKnobs::default(),
    )
    .await
    .expect("恢复路径必须复用引用已归档 sr 的缓存");
    let state = read(&tmp.path().join("ai/state.json"));
    assert_eq!(state["status"], "complete");
}

// ---------------------------------------------------------------------------
// 9. 评审 M-A：兜底伞 —— begin_run 前的结构性失败也写 failed 七键 state
//    （Python except BaseException：graph_repo=None → 跳过 fail_run，仍写 state）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn store_open_failure_umbrella_writes_failed_state() {
    let (tmp, analysis) = setup_root().await;
    // graph/ 被占位为普通文件 → Store::open 必败（Python parity：进 umbrella，run_id=None）。
    std::fs::write(tmp.path().join("graph"), b"not a directory").unwrap();
    let err = run_pipeline(
        test_config(tmp.path(), "http://127.0.0.1:9", true),
        &analysis,
        false,
        &mut PipelineKnobs::default(),
    )
    .await
    .expect_err("must fail");
    assert!(err.to_string().contains("store"), "err={err}");
    let state = read(&tmp.path().join("ai/state.json"));
    assert_eq!(state["status"], "failed");
    assert_eq!(state["viewer_stage_status"], "incomplete");
    assert!(state["graph_run_id"].is_null(), "run 未出生 → null");
    assert_eq!(state["viewer_input_hashes"].as_object().unwrap().len(), 2);
    assert!(state["error"].as_str().unwrap().contains("store"));
}

// ---------------------------------------------------------------------------
// M5-B3：stage hook（per_viewer_ai → audience 顺序钉）+ D7 run_viewer_pipeline
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn stages_hook_reports_per_viewer_ai_then_audience() {
    use std::sync::{Arc, Mutex};
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, _e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    // 只挂载 g1：g2 无 mock → 基于 wiremock 期望验证失败（404）→ viewer stages 完成
    // 但剩余 g1 提交可走 audience。
    mount_audience_ok(&server, &["g1"]).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    let stages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let listener = {
        let stages = stages.clone();
        move |stage: &'static str| stages.lock().unwrap().push(stage.to_string())
    };
    let mut knobs = PipelineKnobs {
        stage: Some(&listener),
        ..PipelineKnobs::default()
    };
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("单观众失败不破坏整体运行");
    let captured = stages.lock().unwrap();
    assert_eq!(
        captured.as_slice(),
        ["per_viewer_ai", "audience"],
        "{captured:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_pipeline_filters_baseline_and_rejects_ghost() {
    use std::sync::{Arc, Mutex};
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    let (e1, _e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_audience_ok(&server, &["g1"]).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);
    let stages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let listener = {
        let stages = stages.clone();
        move |stage: &'static str| stages.lock().unwrap().push(stage.to_string())
    };
    let mut knobs = PipelineKnobs {
        stage: Some(&listener),
        ..PipelineKnobs::default()
    };
    let config = test_config(tmp.path(), &server.uri(), true);
    let result =
        live_core::agent::pipeline::run_viewer_pipeline(config, &analysis, "g1", false, &mut knobs)
            .await
            .expect("单观众运行成功");
    assert_eq!(result["viewer_count"], 1, "{result}");
    assert_eq!(result["viewer_failures"], 0, "{result}");
    // 只有 g1 观众落实到缓存，g2 从不出现。
    assert!(tmp.path().join("ai/perception/viewers/g1.json").exists());
    assert!(!tmp.path().join("ai/perception/viewers/g2.json").exists());

    // 不存在的 uid → 明确错误（编排不去默认观众）
    let error = live_core::agent::pipeline::run_viewer_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        "ghost",
        false,
        &mut knobs,
    )
    .await
    .expect_err("ghost 必须报错");
    assert!(
        error.to_string().contains("baseline 无 viewer ghost"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// P0-1 集成钉（迭代细则 v1 §1）：房间语料 Episode 化收账进 pipeline——
// shared/*.json → _room 命名空间 Episode 落图；重跑幂等（行数不增）；
// 观众 files 零污染（_room 只在图里，绝不成 viewers/ 文件）。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_room_corpus_ingests_into_room_namespace_idempotently() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    // 播种语料：一场回放两行弹幕（跨两片）+ 一条评论。
    let shared = tmp.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("replay_danmaku.json"),
        json!({"records": [{
            "rid": "R9", "title": "回放置景",
            "start_timestamp": 1700000000, "end_timestamp": 1700003600,
            "message_count": 2,
            "messages": [
                {"text": "前排", "uid": "u1", "shard_index": 0, "ts": "1700000005"},
                {"text": "主播晚安", "uid": "u2", "shard_index": 1, "ts": "1700003595"},
            ],
        }]})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        shared.join("room_comments.json"),
        json!({"rows": [{"rpid": "90001", "mid": "8877", "message": "评论区冒泡",
            "ctime": "1700003000", "target_kind": "video", "target_oid": "BV-corpus", "like": "3"}]})
        .to_string(),
    )
    .unwrap();
    let (e1, e2) = episode_ids(tmp.path());
    mount_viewer_ok(
        &server,
        "黄金观众甲",
        viewer_submission("g1", &e1, G1_MENTION, "异环"),
    )
    .await;
    mount_viewer_ok(
        &server,
        "黄金观众乙",
        viewer_submission("g2", &e2, G2_MENTION, "明日方舟"),
    )
    .await;
    mount_audience_ok(&server, &["g1", "g2"]).await;
    let store = open_store(tmp.path());
    seed_entity(&store);
    drop(store);

    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run1");

    let room_rows = |root: &Path| -> i64 {
        open_store(root)
            .conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE viewer_id='_room'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let first = room_rows(tmp.path());
    assert_eq!(first, 3, "弹幕2行+评论1条全部入账: {first}");
    // 观景闸：语料 Episode 的 platform_facts 带 rid/rpid，事件类型可区分。
    let store = open_store(tmp.path());
    let danmaku_facts: String = store
        .conn
        .query_row(
            "SELECT platform_facts_json FROM episodes WHERE viewer_id='_room' AND source='live_danmaku' AND json_extract(platform_facts_json,'$.line_index')=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        danmaku_facts.contains("\"shard_index\":1"),
        "{danmaku_facts}"
    );
    assert!(danmaku_facts.contains("\"rid\":\"R9\""), "{danmaku_facts}");
    drop(store);

    // run2 全缓存恢复（无 LLM 调用）——验收钉①：重复 ingest 撞库行数不增。
    let mut knobs2 = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs2,
    )
    .await
    .expect("run2 must resume from cache");
    assert_eq!(room_rows(tmp.path()), first, "重跑 _room 行数不得增长");

    // 验收钉③：观众 files 零污染——_room 不得落在 viewers/。
    let viewer_files: Vec<_> = std::fs::read_dir(tmp.path().join("viewers"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        !viewer_files.iter().any(|f| f.contains("_room")),
        "viewers/ 不得出现 _room 文件: {viewer_files:?}"
    );
}

// ---------------------------------------------------------------------------
// P0-2 集成钉（迭代细则 v1 §1）：复盘卡 = 规则四数进 ai/recap.json + AI 只命名。
// 两路：命名成功进 naming 键；命名失败卡仍落盘、naming=null、未知行补账。
// ---------------------------------------------------------------------------

fn naming_gate() -> impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static {
    |body: &serde_json::Value| {
        serde_json::to_string(body)
            .map(|text| text.contains("submit_recap_naming"))
            .unwrap_or(false)
    }
}

fn seed_corpus(root: &std::path::Path) {
    let shared = root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("replay_danmaku.json"),
        json!({"records": [
            {"rid": "S1", "start_timestamp": 1000, "end_timestamp": 2000,
             "messages": [{"text": "旧场好", "uid": "A", "shard_index": 0, "ts": "1100"}]},
            {"rid": "S2", "start_timestamp": 3000, "end_timestamp": 4600,
             "messages": [
                 {"text": "晚上好！", "uid": "C", "shard_index": 0, "ts": "3020"},
                 {"text": "来了", "uid": "B", "shard_index": 0, "ts": "3030"},
                 {"text": "晚上好！", "uid": "C", "shard_index": 0, "ts": "4200"},
                 {"text": "晚上好！", "uid": "C", "shard_index": 0, "ts": "4210"},
                 {"text": "好吧", "uid": "A", "shard_index": 0, "ts": "4300"},
             ]},
        ]})
        .to_string(),
    )
    .unwrap();
}

async fn mount_viewers_and_audience(server: &MockServer, root: &std::path::Path) {
    let (e1, e2) = episode_ids(root);
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
    let store = open_store(root);
    seed_entity(&store);
    drop(store);
}

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_recap_card_lands_with_ai_naming() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    seed_corpus(tmp.path());
    mount_viewers_and_audience(&server, tmp.path()).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(naming_gate()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_tool_call(
                "call-n1",
                "submit_recap_naming",
                json!({"submission": {
                    "peak_name": "落雨十分钟",
                    "sentence_name": "晚好三连",
                    "reuse_line": "明天开场把「晚上好」留成仪式句，复用",
                    "cut_advice": "沿复读句「晚上好！」密集段第一刀，留开场互动钩",
                }}),
                None,
            )),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run with naming");

    let card = read(&tmp.path().join("ai").join("recap.json"));
    assert_eq!(card["status"], "ready");
    // 四数：3 人、回来 1/3（A 在 S1 说过）、峰 3 行（4200 窗：4200/4210/4300）、复读「晚上好！」×3
    assert_eq!(card["speakers"], 3);
    assert_eq!(card["returning"]["count"], 1);
    assert_eq!(card["returning"]["base"], 3);
    assert_eq!(card["peak"]["count"], 3);
    assert_eq!(card["repeated"]["text"], "晚上好！");
    let naming = &card["naming"];
    assert_eq!(naming["peak_name"], "落雨十分钟", "卡全貌: {card:?}");
    assert_eq!(naming["sentence_name"], "晚好三连");
    assert!(naming["reuse_line"].as_str().unwrap().contains("复用"));
    assert!(naming["named_at"].as_str().is_some(), "named_at 是程序事实");
}

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_recap_naming_failure_leaves_honest_card() {
    let server = MockServer::start().await;
    let (tmp, analysis) = setup_root().await;
    seed_corpus(tmp.path());
    mount_viewers_and_audience(&server, tmp.path()).await;
    // 命名模型 502 风暴：HTTP_EXTRA_ATTEMPTS 内全灭 → 命名终局失败。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(naming_gate()))
        .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
        .mount(&server)
        .await;

    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri(), true),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("命名失败不得绊管线");

    let card = read(&tmp.path().join("ai").join("recap.json"));
    assert_eq!(card["status"], "ready", "规则卡照落");
    assert!(card["naming"].is_null(), "命名缺位是 null 不是伪造");
    let unknown = card["unknown"].as_array().unwrap();
    assert!(
        unknown.iter().any(|row| row
            .as_str()
            .is_some_and(|text| text.contains("AI 命名未达成"))),
        "未知行要补这笔账: {unknown:?}"
    );
}
