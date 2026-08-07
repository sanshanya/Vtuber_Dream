//! M3-D 金色剧本①②：完整 Agent 装配（prompts + 调查工具 + 终局校验台）经
//! wiremock 剧本端到端跑通，断言请求序列 / reasoning 逐字节回放 / trace 审计面。

use std::path::Path;

use live_core::agent::prompts::{self, trace_run_start};
use live_core::agent::runtime::{AgentRuntime, AttemptPlan, Trace, run_toolcall_agent};
use live_core::agent::specs::{audience_agent_spec, viewer_agent_spec};
use live_core::agent::tools::{AudienceAgentCtx, ResearchService, ViewerAgentCtx};
use live_core::bilibili::BilibiliClient;
use live_core::episodes::{Episode, EpisodeField};
use live_core::graph::store::Store;
use live_core::models::{AudienceSituationSubmission, ViewerPerceptionSubmission};
use serde_json::{Value, json};
use wiremock::MockServer;

// ---------------------------------------------------------------------------
// wiremock 剧本基建（与 agent_runtime.rs 同型；BodyPred 按回合 gate 请求体）
// ---------------------------------------------------------------------------

mod common;
use common::*;

/// G2-A1 白名单注记 fixture（装配面比 Python 冻结面多且只多 verify_videos 的来源）。
/// 路径约定同 m4a：`tests-fixtures/golden/agent_tool_list_note.json`。
fn agent_tool_list_note() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests-fixtures/golden/agent_tool_list_note.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("golden agent_tool_list_note 可读"))
        .expect("golden agent_tool_list_note 是合法 JSON")
}

/// golden 注记 fixture 的数组字段 → Vec<String>（冻结面/白名单共同的比对基准）。
fn note_list(note: &Value, key: &str) -> Vec<String> {
    note[key]
        .as_array()
        .expect("note 字段为数组")
        .iter()
        .map(|item| item.as_str().expect("工具名为字符串").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// 数据基建（与 agent_tools.rs 同型）
// ---------------------------------------------------------------------------

const DEAD_ORIGIN: &str = "http://127.0.0.1:1";

fn mock_client(origin: &str) -> BilibiliClient {
    BilibiliClient::with_origin(origin, origin, "SESSDATA=test", 0.0, 5.0).expect("client")
}

fn fixture_store() -> Store {
    Store::open_with_clock(Path::new(":memory:"), || {
        "2026-08-04T00:00:00+00:00".to_string()
    })
    .expect("store")
}

fn episode(viewer: &str, id: &str, title: &str) -> Episode {
    Episode {
        episode_id: id.to_string(),
        viewer_id: viewer.to_string(),
        source: "video".to_string(),
        event_type: "video".to_string(),
        observed_at: "2026-08-04T00:00:00+00:00".to_string(),
        published_at: "2026-08-01T00:00:00+00:00".to_string(),
        title: title.to_string(),
        url: String::new(),
        bvid: String::new(),
        fields: vec![
            EpisodeField {
                path: "title".to_string(),
                text: title.to_string(),
                kind: "text".to_string(),
            },
            EpisodeField {
                path: "tags[0]".to_string(),
                text: "塞尔达传说".to_string(),
                kind: "platform_tag".to_string(),
            },
        ],
        platform_facts: json!({}),
    }
}

fn plan<'a>(label: &'a str, prompt: &'a str, max_turns: usize) -> AttemptPlan<'a> {
    AttemptPlan {
        label,
        prompt,
        max_turns,
        retries: 0,
        backoff_seconds: 0.0,
        token_budget: None,
    }
}

fn rules() -> Vec<String> {
    vec!["取向优先新内容与互动攻略".to_string()]
}

fn viewer_submission(good: bool) -> Value {
    let evidence: Vec<Value> = if good { vec![json!("m1")] } else { vec![] };
    json!({
        "viewer_id": "v1",
        "profile_summary": "该观众近期集中关注塞尔达系列与开放世界玩法，优先新内容和互动攻略。",
        "mentions": [{
            "mention_id": "m1", "episode_id": "ep-1", "field_path": "title",
            "text": "塞尔达", "start": 1, "end": 4,
            "mention_type": "游戏名", "origin": "explicit",
            "proposed_entity_name": "塞尔达传说", "proposed_entity_type": "游戏",
            "entity_ref": "entity:e1", "confidence": 0.9
        }],
        "entities": [{
            "local_id": "e1", "canonical_name": "塞尔达传说", "entity_type": "游戏",
            "aliases": [], "description": "", "existing_entity_id": null,
            "resolution": "NEW_ENTITY", "evidence_mention_ids": evidence,
            "parent_entity_refs": [], "confidence": 0.8
        }],
        "relations": [],
        "interest_states": [{
            "entity_ref": "entity:e1", "status": "无法判断", "preference": "",
            "aspects": [], "rationale": "", "evidence_mention_ids": ["m1"], "confidence": 0.5
        }],
        "content_preferences": [], "recent_changes": [], "hypotheses": [],
        "conversation_openers": [], "content_ideas": [], "enrichment_targets": [],
        "cautions": [], "leads": []
    })
}

/// ① viewer：调查 → 初稿被拒（空证据）→ 修正接受；reasoning 逐字节回放 + trace 审计面。
#[tokio::test(flavor = "multi_thread")]
async fn golden_viewer_reject_then_accept() {
    let server = MockServer::start().await;
    mount_turn(
        &server,
        |body: &Value| {
            messages_len(2)(body)
                && parallel_calls_disabled()(body)
                && body["messages"][0]["content"].as_str().is_some_and(|s| {
                    s.contains("个人 Perception Agent") && s.contains("项目附加规则")
                })
                && body["messages"][1]["content"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("对下面完整Episode进行开放式"))
        },
        assistant_tool_call(
            "call-1",
            "search_entity_candidates",
            json!({"query": "塞尔达"}),
            Some("先查实体候选"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| {
            messages_len(4)(body)
                && replayed_reasoning("先查实体候选")(body)
                && reasoning_attribution()(body)
        },
        assistant_tool_call(
            "call-2",
            "submit_viewer_perception",
            json!({"submission": viewer_submission(false)}),
            Some("整理后提交初稿"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| {
            messages_len(6)(body)
                && replayed_reasoning("整理后提交初稿")(body)
                && reasoning_attribution()(body)
                && body["messages"].as_array().is_some_and(|m| {
                    m.iter().any(|msg| {
                        msg["content"].as_str().is_some_and(|c| {
                            c.contains("\"accepted\":false")
                                && c.contains("must reference at least one grounded mention")
                        })
                    })
                })
        },
        assistant_tool_call(
            "call-3",
            "submit_viewer_perception",
            json!({"submission": viewer_submission(true)}),
            None,
        ),
    )
    .await;

    let tmp = tempfile::tempdir().unwrap();
    // reqwest blocking 客户端的 build 会在 async 上下文中 drop 内部 runtime（M3-B 已证）
    // → research 经 spawn_blocking 构造；本剧本不触碰其网络面（DEAD_ORIGIN）。
    let research = {
        let output_dir = tmp.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            ResearchService::new(&output_dir, mock_client(DEAD_ORIGIN), 20)
        })
        .await
        .expect("research build")
    };
    let ep = episode("v1", "ep-1", "玩塞尔达旷野之息真上头");
    let mut ctx = ViewerAgentCtx {
        viewer_data: json!({"viewer": {"id": "v1"}}),
        episodes: std::collections::BTreeMap::from([(ep.episode_id.clone(), ep)]),
        research,
        store: fixture_store(),
        slot: Default::default(),
    };
    let mut spec = viewer_agent_spec("v1", &rules());
    let runtime =
        AgentRuntime::for_test(&server.uri(), "custom-reasoning-model", 131_072, true, true);
    let trace_path = tmp.path().join("trace.jsonl");
    let mut trace = Trace::new(Some(trace_path.clone()));
    trace_run_start(
        &mut trace,
        &spec.name,
        "custom-reasoning-model",
        "submit_viewer_perception",
        "live_core::models::ViewerPerceptionSubmission",
    );
    let input = json!({"viewer": {"id": "v1"}, "episodes": [{"episode_id": "ep-1"}]});
    let prompt = prompts::viewer_user_prompt(&input);
    let outcome = run_toolcall_agent::<ViewerAgentCtx, ViewerPerceptionSubmission>(
        &runtime,
        &mut spec,
        plan("观众 v1", &prompt, 8),
        &mut ctx,
        &mut trace,
    )
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            spawn_drop(ctx).await;
            panic!("viewer run rejected: {err}");
        }
    };

    // 评审5-M2：断言前先取数再 spawn_drop——闭窗 blocking client 的 async unwind-drop。
    let slot_value = ctx.slot.value.clone();
    let slot_errors = ctx.slot.validation_errors.clone();
    spawn_drop(ctx).await;
    assert_eq!(outcome.final_output, "accepted");
    assert_eq!(outcome.submission.viewer_id, "v1");
    assert_eq!(outcome.submission.entities.len(), 1);
    assert_eq!(outcome.submission.interest_states.len(), 1);
    let accepted = slot_value.expect("终局接受必落槽");
    assert!(slot_errors.is_empty());
    assert_eq!(accepted["entities"][0]["resolution"], json!("NEW_ENTITY"));

    drop(trace);
    let trace_text = std::fs::read_to_string(&trace_path).unwrap();
    assert!(
        trace_text.contains("\"event\":\"run_start\""),
        "{trace_text}"
    );
    assert!(trace_text.contains("m3d-2026-08-05"), "{trace_text}");
    assert!(
        trace_text.contains("2026-08-05.v1"),
        "R1-4 工具规格版本串必须随 run_start 入 trace（G2-A1 入装配 ≥ v2）：{trace_text}"
    );
    assert!(
        trace_text.contains("ViewerPerceptionSubmission"),
        "{trace_text}"
    );
    assert!(trace_text.contains("elapsed_ms"), "{trace_text}");
    // reasoning 内容永不入 trace（红线）
    assert!(!trace_text.contains("先查实体候选"), "{trace_text}");
    assert!(!trace_text.contains("整理后提交初稿"), "{trace_text}");
}

/// ② audience：工具按需核验 → 一次提交接受。
#[tokio::test(flavor = "multi_thread")]
async fn golden_audience_happy_path() {
    let server = MockServer::start().await;
    mount_turn(
        &server,
        |body: &Value| {
            messages_len(2)(body)
                && parallel_calls_disabled()(body)
                && body["messages"][0]["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("整体 Situation Agent"))
        },
        assistant_tool_call(
            "call-1",
            "query_graph",
            // schema 参数名是 query（评审 graph-m3：此前误写 needle → 全表路径逃过端到端钉）
            json!({"query": "塞尔达"}),
            Some("先看图"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| messages_len(4)(body) && replayed_reasoning("先看图")(body),
        assistant_tool_call(
            "call-2",
            "get_viewer_analysis",
            json!({"viewer_id": "177"}),
            Some("核验单人"),
        ),
    )
    .await;
    mount_turn(
        &server,
        |body: &Value| messages_len(6)(body) && replayed_reasoning("核验单人")(body),
        assistant_tool_call(
            "call-3",
            "submit_audience_situation",
            json!({"submission": {
                "executive_summary": "观众围绕塞尔达形成单一高粘社区，近期对新作内容需求上升。",
                // 简报带真实入库 episode 引用（ep-a1 已于本 fixture upsert）。
                "front_brief": {"sentences": [{
                    "text": "舰长甲近期围绕《塞尔达》开荒实况持续活跃，新作需求升温。",
                    "episode_refs": ["ep-a1"],
                    "coverage_time_range": ["2026-08-01", "2026-08-04"]
                }]},
                "audience_structure": [],
                "interest_graph": [{
                    "entity_id": "ent1", "entity": "塞尔达传说", "entity_type": "游戏",
                    "parent_entities": [], "angles": [], "viewer_ids": ["177"],
                    "status": "无法判断", "confidence": 0.6,
                    "evidence_summary": "", "evidence_mention_ids": ["m-a1"]
                }],
                "communities": [], "situations": [], "content_opportunities": [],
                "individual_highlights": [], "content_calendar": [],
                "data_gaps": [], "safety_notes": [], "leads": []
            }}),
            Some("聚合后提交"),
        ),
    )
    .await;

    let tmp = tempfile::tempdir().unwrap();
    let store = fixture_store();
    store.conn.execute(
        "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
         VALUES('ent1','塞尔达传说','塞尔达传说','游戏','','ai_semantic','{}','t0','t1')",
        [],
    ).unwrap();
    // references 的 entities 桶 JOIN nodes（graph 集成 M1）：镜像节点必须同生
    store
        .upsert_node(
            "ent1",
            "Entity",
            "塞尔达传说",
            &json!({}),
            "ai_semantic",
            None,
        )
        .unwrap();
    store
        .upsert_episode(&episode("177", "ep-a1", "塞尔达开荒实况"))
        .unwrap();
    // mentions.run_id REFERENCES graph_runs：先开 run 收口外键（与 agent_tools 同型）
    store
        .begin_run_fixed("run-1", "2026-08-04T00:00:00+00:00", "model-x")
        .unwrap();
    store.conn.execute(
        "INSERT INTO mentions(mention_id,episode_id,viewer_id,field_path,text,start_offset,end_offset,mention_type,origin,proposed_entity_name,proposed_entity_type,confidence,run_id,created_at) \
         VALUES('m-a1','ep-a1','177','title','塞尔达',0,3,'游戏名','explicit','塞尔达传说','游戏',0.9,'run-1','2026-08-04T00:00:00+00:00')",
        [],
    ).unwrap();

    let mut viewer_analyses = serde_json::Map::new();
    viewer_analyses.insert("177".to_string(), json!({"profile_summary": "舰长甲"}));
    let research = {
        let output_dir = tmp.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            ResearchService::new(&output_dir, mock_client(DEAD_ORIGIN), 20)
        })
        .await
        .expect("research build")
    };
    let mut ctx = AudienceAgentCtx {
        viewer_analyses,
        research,
        store,
        graph_run_id: None,
        slot: Default::default(),
    };
    let mut spec = audience_agent_spec(&rules());
    let runtime =
        AgentRuntime::for_test(&server.uri(), "custom-reasoning-model", 131_072, true, true);
    let trace_path = tmp.path().join("trace.jsonl");
    let mut trace = Trace::new(Some(trace_path.clone()));
    trace_run_start(
        &mut trace,
        &spec.name,
        "custom-reasoning-model",
        "submit_audience_situation",
        "live_core::models::AudienceSituationSubmission",
    );
    let prompt = prompts::audience_user_prompt(&json!({"viewers": ["177"]}));
    let outcome = run_toolcall_agent::<AudienceAgentCtx, AudienceSituationSubmission>(
        &runtime,
        &mut spec,
        plan("整体Situation", &prompt, 8),
        &mut ctx,
        &mut trace,
    )
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            spawn_drop(ctx).await;
            panic!("audience run rejected: {err}");
        }
    };

    let slot_value = ctx.slot.value.clone();
    spawn_drop(ctx).await;
    assert_eq!(outcome.final_output, "accepted");
    assert_eq!(outcome.submission.interest_graph.len(), 1);
    let accepted = slot_value.expect("终局接受必落槽");
    assert_eq!(accepted["interest_graph"][0]["entity_id"], json!("ent1"));
    // 带有效 episode 引用的简报全链落槽（终局闭包过 episodes 桶闭包校验）。
    assert_eq!(
        accepted["front_brief"]["sentences"][0]["episode_refs"][0],
        json!("ep-a1"),
        "简报 refs 原样入槽：{accepted}"
    );
    assert_eq!(outcome.submission.front_brief.sentences.len(), 1);
    drop(trace);
    let trace_text = std::fs::read_to_string(&trace_path).unwrap();
    assert!(
        trace_text.contains("AudienceSituationSubmission"),
        "{trace_text}"
    );
    assert!(
        trace_text.contains("2026-08-05.v1"),
        "R1-4 工具规格版本串必须随 run_start 入 trace（G2-A1 入装配 ≥ v2）：{trace_text}"
    );
    assert!(!trace_text.contains("核验单人"), "{trace_text}");
}

/// 组装 parity：指令拼接 / 用户前缀 / 紧凑 JSON 与 Python `_json` 同形。
#[test]
fn prompts_assembly_and_spec_parity() {
    let instructions = prompts::viewer_instructions(&rules());
    assert!(instructions.contains("个人 Perception Agent"));
    assert!(instructions.contains("\n项目附加规则：\n- 取向优先新内容与互动攻略"));
    assert!(!instructions.starts_with('\n'));

    let instructions = prompts::audience_instructions(&rules());
    assert!(instructions.contains("整体 Situation Agent"));

    let prompt = prompts::viewer_user_prompt(&json!({"viewer": {"id": "v1"}}));
    assert!(prompt.starts_with(prompts::VIEWER_USER_PROMPT_PREFIX));
    assert!(prompt.ends_with("{\"viewer\":{\"id\":\"v1\"}}")); // ensure_ascii=False 紧凑

    let prompt = prompts::audience_user_prompt(&json!({"a": "异环"}));
    assert!(prompt.ends_with("{\"a\":\"异环\"}"));

    // -------------------------------------------------------------------------
    // G2-A1 白名单（golden 比对基准 = tests-fixtures/golden/agent_tool_list_note.json）：
    // 本装配比 Python 冻结面多且只多 `verify_videos` —— design 红线② 的批形核验
    // 原语（docs/2026-08-03-rust-rewrite-design.md:182，S0 实测：per-video 单验把每
    // 观众滚到 15+ 轮、input 峰值 65k/轮），Python 侧（tools.py @function_tool 全集）
    // 无此明文对象，无法进冻结面——故按 2026-08-04-g2-gate-ruling §5「入装配」。
    // 比对规则 = 「冻结四件逐字在 + 唯一白名单增量 verify_videos + 除此之外无其他
    // 增量」：本块不变会红、未来在装配里混入新工具也会红——白名单是防静默漂移的钩。
    let note = agent_tool_list_note();
    let frozen: Vec<String> = note_list(&note, "viewer_python_frozen_tools");
    let whitelist: Vec<String> = note_list(&note, "viewer_whitelist_increment");
    assert_eq!(
        frozen,
        [
            "search_entity_candidates",
            "search_bilibili_videos",
            "get_bilibili_video",
            "submit_viewer_perception"
        ],
        "Python 冻结四件 = 注记 fixture 基准，不得漂移"
    );
    assert_eq!(
        whitelist,
        ["verify_videos"],
        "唯一白名单增量 = verify_videos（设计红线②），不得增改"
    );

    let spec = viewer_agent_spec("v1", &rules());
    assert_eq!(spec.name, "Viewer Grounded Perception v1");
    let names: Vec<String> = spec.tools.iter().map(|t| t.name.clone()).collect();
    // viewer 装配 = 冻结调查三件 + 白名单 verify_videos + 冻结终局（5 件 = 4 冻结 + 1 白名单）。
    let mut expected_viewer: Vec<String> = frozen[..3].to_vec();
    expected_viewer.extend(whitelist.clone());
    expected_viewer.push(frozen[3].clone());
    assert_eq!(
        names, expected_viewer,
        "装配表必须严格等于「冻结+白名单」公式"
    );
    assert_eq!(
        names.len(),
        5,
        "viewer 工具清单恰好 5 件（4 冻结 + verify_videos 白名单）"
    );
    assert!(!spec.tools[3].terminal);
    assert!(spec.tools[4].terminal);

    let spec = audience_agent_spec(&rules());
    assert_eq!(spec.name, "Audience Situation Intelligence");
    let names: Vec<String> = spec.tools.iter().map(|t| t.name.clone()).collect();
    let audience_frozen: Vec<String> = note_list(&note, "audience_python_frozen_tools");
    assert_eq!(names, audience_frozen, "audience 装配仍 4+1 冻结，逐字不改");
    assert!(spec.tools[4].terminal);
    assert!(
        !names.contains(&"verify_videos".to_string()),
        "audience 提示面不得出现 verify_videos（design 红线② 语境是 viewer 校验场景）"
    );
}

/// G2-A1 新钉：调查工具面（不含终局）清单——
/// - viewer 面 = 冻结三调查 + 唯一白名单增量 verify_videos（恰 4 件，比冻结面多且只多这一个）；
/// - audience 面 = 冻结四件逐字，绝不出现 verify_videos（design 红线② 语境是 viewer 校验场景）。
///
/// 终局不入调查集（submit_* 由 specs 装配器 push），两处计数对账 4 起钉。
#[test]
fn g2_a1_investigation_surface_whitelist_and_audience_isolation() {
    use live_core::agent::tools::{audience_investigation_tools, viewer_investigation_tools};

    let note = agent_tool_list_note();
    let frozen: Vec<String> = note_list(&note, "viewer_python_frozen_tools");
    let whitelist: Vec<String> = note_list(&note, "viewer_whitelist_increment");
    let audience_frozen: Vec<String> = note_list(&note, "audience_python_frozen_tools");

    let viewer_inv: Vec<String> = viewer_investigation_tools()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    // viewer 调查面 = 冻结前 3 件（调查三件）+ 白名单增量，恰 4 件。
    let mut expected_viewer_inv: Vec<String> = frozen[..3].to_vec();
    expected_viewer_inv.extend(whitelist.clone());
    assert_eq!(viewer_inv, expected_viewer_inv);
    assert_eq!(viewer_inv.len(), 4);
    assert_eq!(
        viewer_inv.iter().filter(|n| **n == "verify_videos").count(),
        1,
        "verify_videos 在 viewer 提示面恰好出现一次"
    );

    let audience_inv: Vec<String> = audience_investigation_tools()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    // audience 调查面 4 件 + 终局 = 4+1；不含白名单增量。
    assert_eq!(
        &audience_inv[..],
        &audience_frozen[..4],
        "audience 调查面 = 冻结四件的 前四件（不含终局）"
    );
    assert_eq!(audience_inv.len(), 4);
    assert!(
        !audience_inv.iter().any(|n| whitelist.contains(n)),
        "audience 调查面不得出现 viewer 白名单增量 {whitelist:?}"
    );
}

/// blocking 客户端的 drop 禁在 async 上下文（M3-B 已证）；测试收尾统一走这里。
async fn spawn_drop<T: Send + 'static>(value: T) {
    tokio::task::spawn_blocking(move || drop(value))
        .await
        .expect("drop task");
}

/// 基础设施失败走 Fatal 通道——不白标为模型可修正的校验拒收，
/// 且槽位（submission/validation_errors）不被污染（Python SDK tool-error 通道镜像）。
#[test]
fn terminal_fatal_channel_on_store_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let store = fixture_store();
    // 毒化：删 entities 表 → entity_exists 查询必败
    store.conn.execute("DROP TABLE entities", []).unwrap();
    let mut ctx = ViewerAgentCtx {
        viewer_data: json!({"viewer": {"id": "v1"}}),
        episodes: Default::default(),
        research: ResearchService::new(tmp.path(), mock_client(DEAD_ORIGIN), 20),
        store,
        slot: live_core::agent::runtime::SubmissionSlot {
            value: Some(json!({"legacy": "kept"})),
            validation_errors: vec!["kept".to_string()],
        },
    };
    let mut tool = live_core::agent::specs::viewer_terminal_tool();
    let out = (tool.handler)(&mut ctx, &json!({"submission": viewer_submission(true)}));
    assert!(
        out.get("accepted").is_none(),
        "Fatal 不得有 accepted 键: {out}"
    );
    assert!(
        out["error"]
            .as_str()
            .unwrap()
            .contains("entity lookup failed"),
        "{out}"
    );
    // 槽位不污染：既有 value/errors 原样保留（Python validation_errors 不覆写语义）
    assert_eq!(ctx.slot.value, Some(json!({"legacy": "kept"})));
    assert_eq!(ctx.slot.validation_errors, vec!["kept".to_string()]);
}

/// graph m4：Send 界是 M4 并发装配的编译期钉。
#[test]
fn agent_tools_are_send_for_m4_concurrency() {
    fn assert_send<T: Send>() {}
    assert_send::<live_core::agent::runtime::AgentTool<ViewerAgentCtx>>();
    assert_send::<live_core::agent::runtime::AgentTool<AudienceAgentCtx>>();
    assert_send::<live_core::agent::runtime::AgentTool<live_core::agent::probe::ProbeContext>>();
}
