//! M4.x-B 验收（kickoff G3）的 G2 表形态（design §9.2 行 254）：线索→discovery_leads
//! 表→下轮上下文摘要（viewer/audience 双注入面），同输入重跑账本不增行，强制重跑
//! 作用域 annex。剧本基座 = tests-fixtures/m4a/viewer_root（两观众），与 pipeline_run.rs 同族。
mod common;

use std::path::Path;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{BodyPred, assistant_tool_call, messages_len};
use live_core::agent::pipeline::{PipelineKnobs, run_pipeline, viewer_input_bundle};
use live_core::config::{
    AgentRuntimeConfig, AiConfig, BilibiliConfig, CollectionConfig, Config, PeerDiscoveryConfig,
    PerceptionConfig, ReasoningConfig,
};
use live_core::episodes::baseline::build_factual_baseline;
use live_core::graph::store::Store;
use live_core::leads;

const SEED_ENTITY: &str = "ent-demo";
const G1_MENTION: (i64, i64, &str) = (1, 3, "异环");
const G2_MENTION: (i64, i64, &str) = (12, 16, "明日方舟");
const LEAD_LOCATOR: &str = "BV1TEST111aaaa";

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

fn test_config(root: &Path, uri: &str) -> Config {
    Config {
        source: root.join("config.yaml"),
        project_name: "m4x".into(),
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
            model: "m4x-model".into(),
            timeout_seconds: 5.0,
            max_output_tokens: 4096,
            reasoning: ReasoningConfig {
                enabled: false,
                effort: "high".into(),
                replay_content: true,
            },
            agent: AgentRuntimeConfig {
                max_turns: 4,
                resume: true,
                local_trace: false,
                run_retries: 0,
                retry_backoff_seconds: 0.0,
                viewer_token_budget: 200_000,
            },
            search_results_per_query: 20,
            rules: vec!["取向优先新内容与互动攻略".into()],
        },
        report_title: "t".into(),
    }
}

/// G2 表形态：读取面唯一源 = discovery_leads 表（写账序）。
fn table_rows(root: &Path) -> Vec<leads::LedgerRow> {
    let store = Store::open(&root.join("graph/perception.sqlite3")).expect("store opens");
    leads::read_rows(&store).expect("lead rows read")
}

fn ep_id(root: &Path, uid: &str) -> String {
    let analysis = build_factual_baseline(root, 1000).unwrap();
    let raw: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(format!("viewers/{uid}.json"))).unwrap(),
    )
    .unwrap();
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
        "m4x-model",
        "chat_completions",
        &json!({"enabled": false, "effort": "high", "replay_content": true}),
        &["取向优先新内容与互动攻略".to_string()],
        1000,
    )
    .episodes[0]
        .episode_id
        .clone()
}

/// g1 提交带一条 video 型 lead（kickoff 验收物）；g2 无 leads。
fn viewer_submission(uid: &str, episode_id: &str, with_lead: bool) -> Value {
    let mention = if uid == "g1" { G1_MENTION } else { G2_MENTION };
    let entity_name = if uid == "g1" {
        "异环"
    } else {
        "明日方舟"
    };
    let leads = if with_lead {
        json!([{
            "type": "video", "locator": LEAD_LOCATOR,
            "motivation": "同名作品官号实况可校验兴趣强度与更新节奏。",
            "expected_signal": "若收藏含官号视频则视为强兴趣证据。",
            "priority": "high", "evidence_ids": ["m1"]
        }])
    } else {
        json!([])
    };
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
        "cautions": [], "leads": leads
    })
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
    |body: &Value| {
        messages_len(2)(body)
            && body["messages"][1]["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("基于下面的全员索引"))
    }
}

// audience 提交引用播种实体 + 各观众实测 mention id（与 demo-1 剧本同构）。
fn mention_ids(root: &Path) -> (String, String) {
    let span = |uid: &str| live_core::models::MentionSpan {
        mention_id: "m1".into(),
        episode_id: ep_id(root, uid),
        field_path: "title".into(),
        text: if uid == "g1" {
            "异环".into()
        } else {
            "明日方舟".into()
        },
        start: if uid == "g1" { 1 } else { 12 },
        end: if uid == "g1" { 3 } else { 16 },
        mention_type: "作品名".into(),
        origin: "explicit".into(),
        proposed_entity_name: String::new(),
        proposed_entity_type: String::new(),
        entity_ref: "entity:e1".into(),
        confidence: 0.9,
    };
    (
        live_core::graph::store::mention_id_of("g1", &span("g1")),
        live_core::graph::store::mention_id_of("g2", &span("g2")),
    )
}

fn audience_submission(root: &Path) -> Value {
    let (m1, m2) = mention_ids(root);
    json!({
        "executive_summary": "观众分别围绕演示作品形成独立兴趣焦点，结构简单清晰可追溯。",
        "audience_structure": [],
        "interest_graph": [{
            "entity_id": SEED_ENTITY, "entity": "演示聚合实体", "entity_type": "游戏",
            "parent_entities": [], "angles": [], "viewer_ids": ["g1", "g2"],
            "status": "无法判断", "confidence": 0.6, "evidence_summary": "",
            "evidence_mention_ids": [m1, m2]
        }],
        "communities": [], "situations": [], "content_opportunities": [],
        "individual_highlights": [], "content_calendar": [],
        "data_gaps": [], "safety_notes": [],
        // MXA-6（r2-F7/r5-F3）：AUDIENCE_VIEWER_ID 支路必触线——一条 search lead。
        "leads": [{
            "type": "search", "locator": "异环 实机",
            "motivation": "全员聚合实体缺少实机语料校各自推断。",
            "expected_signal": "搜索结果出现近期实机视频即全员兴趣在升温。",
            "priority": "medium", "evidence_ids": []
        }]
    })
}

/// 挂载恰好 hit `times` 次的回合 mock。
async fn mount_n(
    server: &MockServer,
    predicate: impl Fn(&Value) -> bool + Send + Sync + 'static,
    response: Value,
    times: u32,
) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(BodyPred(predicate))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(times as u64)
        .mount(server)
        .await;
}

async fn prompt_bodies(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .filter_map(|req| {
            serde_json::from_slice::<Value>(&req.body).ok()?["messages"][1]["content"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

async fn setup() -> (MockServer, tempfile::TempDir, Value) {
    let server = MockServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&m4a_root(), tmp.path());
    let analysis = build_factual_baseline(tmp.path(), 1000).unwrap();
    let store = Store::open(&tmp.path().join("graph/perception.sqlite3")).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
             VALUES('ent-demo','演示聚合实体','演示聚合实体','游戏','','ai_semantic','{}','t0','t1')",
            [],
        )
        .unwrap();
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
    drop(store);
    (server, tmp, analysis)
}

/// 挂载首轮（观众×2 + audience×1）+ force 轮再 ×1 共 2 次。
async fn mount_all(server: &MockServer, root: &Path, times: u32) {
    mount_n(
        server,
        name_gate("黄金观众甲"),
        assistant_tool_call(
            "call-v1",
            "submit_viewer_perception",
            json!({"submission": viewer_submission("g1", &ep_id(root, "g1"), true)}),
            None,
        ),
        times,
    )
    .await;
    mount_n(
        server,
        name_gate("黄金观众乙"),
        assistant_tool_call(
            "call-v2",
            "submit_viewer_perception",
            json!({"submission": viewer_submission("g2", &ep_id(root, "g2"), false)}),
            None,
        ),
        times,
    )
    .await;
    mount_n(
        server,
        audience_gate(),
        assistant_tool_call(
            "call-a1",
            "submit_audience_situation",
            json!({"submission": audience_submission(root)}),
            None,
        ),
        times,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn leads_end_to_end_and_rerun_ledger_stable() {
    let (server, tmp, analysis) = setup().await;
    mount_all(&server, tmp.path(), 1).await;
    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri()),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run1 complete");
    assert_eq!(result["status"], "complete");

    // ── 账本行：两条（viewer 一条 + audience 一条，MXA-6），字段按 kickoff 契约 ──
    let rows = table_rows(tmp.path());
    assert_eq!(rows.len(), 2);
    let row = &rows[0];
    assert_eq!(row.lead_type, "video");
    assert_eq!(row.locator, LEAD_LOCATOR);
    assert_eq!(row.viewer_id, "g1");
    assert_eq!(row.status, leads::LeadStatus::PendingApproval);
    assert_eq!(
        row.dedupe_key,
        leads::dedupe_key(&live_core::models::Lead {
            lead_type: "video".into(),
            locator: LEAD_LOCATOR.into(),
            motivation: "x".into(),
            expected_signal: "x".into(),
            priority: "x".into(),
            evidence_ids: vec![],
        })
    );
    let audience_row = &rows[1];
    assert_eq!(audience_row.lead_type, "search");
    assert_eq!(audience_row.viewer_id, leads::AUDIENCE_VIEWER_ID);
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join("ai/state.json")).unwrap())
            .unwrap();
    assert_eq!(
        row.first_seen_run_id,
        state["graph_run_id"].as_str().unwrap()
    );
    assert_eq!(
        audience_row.first_seen_run_id,
        state["graph_run_id"].as_str().unwrap()
    );

    // ── G3①：同 run 内 audience 提示面已读到本轮 pending（写先于拼装）──
    let bodies = prompt_bodies(&server).await;
    let audience_body = bodies
        .iter()
        .find(|b| b.starts_with("基于下面的全员索引"))
        .expect("audience prompt");
    assert!(
        audience_body.contains("[lead_ledger]") && audience_body.contains("pending=1"),
        "audience prompt 缺账本 annex：{audience_body}"
    );
    // audience 自己的 lead 在同 run 的 annex 中不可见（audience annex 在终局提交前拼装）——
    // 同 run 只能见 viewer 行的贡献；观众与整体两层账被 by_type 全反射于下轮。
    assert!(
        audience_body.contains("by_type={video: 1}"),
        "{audience_body}"
    );
    // 首轮 viewer 提示面无账本（账本空 → 零面世；M4 字节 parity 免疫）
    let g1_body = bodies
        .iter()
        .find(|b| b.starts_with("对下面完整Episode") && b.contains("黄金观众甲"))
        .expect("viewer g1 prompt");
    assert!(!g1_body.contains("[lead_ledger]"), "{g1_body}");

    // ── MXA-5 探针 ②（表形态）：账本漂移（行状态被人工翻动 + 删账毁掉幂等锚）
    // 不影响缓存命中：在「同输入 + 漂移账本」下 run2 不得发 LLM ──
    let mut drifted = rows.clone();
    drifted[0].status = leads::LeadStatus::Approved;
    drifted[0].yield_count = 3;
    {
        let store = Store::open(&tmp.path().join("graph/perception.sqlite3")).unwrap();
        store.update_lead_row(&drifted[0]).expect("drift ok");
    }

    let llm_count = server.received_requests().await.expect("requests").len();
    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri()),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run2 cache-hit complete");
    assert_eq!(result["status"], "complete");
    let drifted_back = table_rows(tmp.path());
    assert_eq!(
        drifted_back.len(),
        2,
        "账本漂移 dedupe：状态行保留 + 同键不增生"
    );
    assert_eq!(
        drifted_back[0].status,
        leads::LeadStatus::Approved,
        "人工翻动状态不被 record 回写清洗"
    );
    assert_eq!(drifted_back[0].yield_count, 3);
    assert_eq!(
        server.received_requests().await.expect("requests").len(),
        llm_count,
        "缓存命中轮不应再发 LLM 请求（账本漂移不失效 input_hash）"
    );

    // ── MXA-5 探针 ①（表形态）：删账后缓存命中重跑 → 账本被**重写回来**（补写真的发生）──
    {
        let store = Store::open(&tmp.path().join("graph/perception.sqlite3")).unwrap();
        store
            .conn
            .execute("DELETE FROM discovery_leads", [])
            .expect("wipe ledger");
    }
    assert_eq!(table_rows(tmp.path()).len(), 0, "删账前置态");
    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri()),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run3 cache-hit complete");
    assert_eq!(result["status"], "complete");
    let rebuilt = table_rows(tmp.path());
    assert_eq!(rebuilt.len(), 2, "账本被缓存命中路径补写回来");
    assert_eq!(rebuilt[0].dedupe_key, rows[0].dedupe_key);
    assert_eq!(rebuilt[1].dedupe_key, rows[1].dedupe_key);
    // 补写的行是新一轮 pending_approval（漂移状态并未硬保：删账=重新开局）
    assert_eq!(rebuilt[0].status, leads::LeadStatus::PendingApproval);
}

#[tokio::test(flavor = "multi_thread")]
async fn force_rerun_scoped_annex_and_ledger_deduped() {
    let (server, tmp, analysis) = setup().await;
    mount_all(&server, tmp.path(), 2).await;
    let mut knobs = PipelineKnobs::default();
    run_pipeline(
        test_config(tmp.path(), &server.uri()),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("run1 complete");
    let mut knobs = PipelineKnobs::default();
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri()),
        &analysis,
        true,
        &mut knobs,
    )
    .await
    .expect("run2 forced complete");
    assert_eq!(result["status"], "complete");

    // dedupe：强制重跑（viewer+audience 重跑 + leads 重复写入）→ 账本仍 2 行
    assert_eq!(table_rows(tmp.path()).len(), 2);

    // 作用域 annex：g1 看 own_pending=1、g2 看 0；audience 全局 annex 再次在场
    let bodies = prompt_bodies(&server).await;
    let g1_forced = bodies
        .iter()
        .filter(|b| b.starts_with("对下面完整Episode") && b.contains("黄金观众甲"))
        .count();
    assert_eq!(g1_forced, 2, "首轮无声 + 强制轮带账本");
    let forced_g1 = bodies
        .iter()
        .rev()
        .find(|b| b.starts_with("对下面完整Episode") && b.contains("黄金观众甲"))
        .unwrap();
    assert!(
        forced_g1.contains("[lead_ledger] viewer=g1 own_pending=1"),
        "{forced_g1}"
    );
    let forced_g2 = bodies
        .iter()
        .rev()
        .find(|b| b.starts_with("对下面完整Episode") && b.contains("黄金观众乙"))
        .unwrap();
    assert!(
        forced_g2.contains("[lead_ledger] viewer=g2 own_pending=0"),
        "{forced_g2}"
    );
}

/// r2-F8 钉：checkpoint 通道可失败——Err 必须吞掉记 progress，不打断本观众队列。
#[tokio::test(flavor = "multi_thread")]
async fn checkpoint_error_rings_progress_and_never_breaks_runs() {
    use std::sync::{Arc, Mutex};
    let (server, tmp, analysis) = setup().await;
    mount_all(&server, tmp.path(), 1).await;
    let messages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let sink = messages.clone();
    let progress = move |msg: &str| sink.lock().unwrap().push(msg.to_string());
    let mut checkpoint = || Err::<(), String>("报告刷新演示态失败".to_string());
    let mut knobs = PipelineKnobs {
        progress: Some(&progress),
        checkpoint: Some(&mut checkpoint),
        ..PipelineKnobs::default()
    };
    let result = run_pipeline(
        test_config(tmp.path(), &server.uri()),
        &analysis,
        false,
        &mut knobs,
    )
    .await
    .expect("checkpoint Err 不中断管线（r2-F8）");
    assert_eq!(result["status"], "complete");
    let captured = messages.lock().unwrap();
    let rings: Vec<&String> = captured
        .iter()
        .filter(|m| m.contains("checkpoint 失败"))
        .collect();
    assert_eq!(
        rings.len(),
        2,
        "双观众各一次 checkpoint 应用点 → 两声铃：{rings:?}"
    );
    assert!(
        rings[0].contains("报告刷新演示态失败"),
        "铃必须带登记错误文案：{}",
        rings[0]
    );
}
