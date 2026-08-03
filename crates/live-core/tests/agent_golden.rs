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
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

// ---------------------------------------------------------------------------
// wiremock 剧本基建（与 agent_runtime.rs 同型；BodyPred 按回合 gate 请求体）
// ---------------------------------------------------------------------------

struct BodyPred<F>(F)
where
    F: Fn(&Value) -> bool + Send + Sync;

impl<F> Match for BodyPred<F>
where
    F: Fn(&Value) -> bool + Send + Sync,
{
    fn matches(&self, request: &Request) -> bool {
        match serde_json::from_slice::<Value>(&request.body) {
            Ok(body) => (self.0)(&body),
            Err(_) => false,
        }
    }
}

fn assistant_tool_call(id: &str, name: &str, args: Value, reasoning: Option<&str>) -> Value {
    json!({
        "id": format!("chatcmpl-{id}"),
        "object": "chat.completion",
        "created": 1_700_000_000,
        "model": "custom-reasoning-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning_content": reasoning,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": args.to_string()},
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
}

async fn mount_turn(
    server: &MockServer,
    predicate: impl Fn(&Value) -> bool + Send + Sync + 'static,
    response: Value,
) {
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .and(BodyPred(predicate))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

/// reasoning 归属钉（评审5-m2）：有 reasoning 的 assistant 消息必须紧跟自己 tool_calls 的结果。
fn reasoning_attribution() -> impl Fn(&Value) -> bool + Send + Sync {
    |body: &Value| {
        body["messages"].as_array().is_some_and(|msgs| {
            msgs.iter().enumerate().all(|(index, message)| {
                if message["role"].as_str() != Some("assistant")
                    || message["reasoning_content"].is_null()
                {
                    return true;
                }
                let id = message["tool_calls"][0]["id"].as_str();
                id.is_some()
                    && msgs.get(index + 1).is_some_and(|tool| {
                        tool["role"].as_str() == Some("tool") && tool["tool_call_id"].as_str() == id
                    })
            })
        })
    }
}

/// parallel_tool_calls=false 的 wire 钉（评审5-m1；剧本计数 messages_len(2k) 的隐性前提）。
fn parallel_calls_disabled() -> impl Fn(&Value) -> bool + Send + Sync {
    |body: &Value| body["parallel_tool_calls"] == serde_json::Value::Bool(false)
}

fn replayed_reasoning(expect: &str) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    let expect = expect.to_string();
    move |body: &Value| {
        body["messages"].as_array().is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m["role"].as_str() == Some("assistant")
                    && m["reasoning_content"].as_str() == Some(expect.as_str())
            })
        })
    }
}

fn messages_len(n: usize) -> impl Fn(&Value) -> bool + Send + Sync + 'static {
    move |body: &Value| body["messages"].as_array().is_some_and(|m| m.len() == n)
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

    assert_eq!(outcome.final_output, "accepted");
    assert_eq!(outcome.submission.viewer_id, "v1");
    assert_eq!(outcome.submission.entities.len(), 1);
    assert_eq!(outcome.submission.interest_states.len(), 1);
    assert!(ctx.slot.value.is_some());
    assert!(ctx.slot.validation_errors.is_empty());
    let accepted = ctx.slot.value.clone().unwrap();
    assert_eq!(accepted["entities"][0]["resolution"], json!("NEW_ENTITY"));

    drop(trace);
    let trace_text = std::fs::read_to_string(&trace_path).unwrap();
    assert!(
        trace_text.contains("\"event\":\"run_start\""),
        "{trace_text}"
    );
    assert!(trace_text.contains("m3d-2026-08-04"), "{trace_text}");
    assert!(
        trace_text.contains("ViewerPerceptionSubmission"),
        "{trace_text}"
    );
    assert!(trace_text.contains("elapsed_ms"), "{trace_text}");
    // reasoning 内容永不入 trace（红线 R5）
    assert!(!trace_text.contains("先查实体候选"), "{trace_text}");
    assert!(!trace_text.contains("整理后提交初稿"), "{trace_text}");

    spawn_drop(ctx).await;
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

    assert_eq!(outcome.final_output, "accepted");
    assert_eq!(outcome.submission.interest_graph.len(), 1);
    let accepted = ctx.slot.value.clone().unwrap();
    assert_eq!(accepted["interest_graph"][0]["entity_id"], json!("ent1"));
    drop(trace);
    let trace_text = std::fs::read_to_string(&trace_path).unwrap();
    assert!(
        trace_text.contains("AudienceSituationSubmission"),
        "{trace_text}"
    );
    assert!(!trace_text.contains("核验单人"), "{trace_text}");

    spawn_drop(ctx).await;
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

    let spec = viewer_agent_spec("v1", &rules());
    assert_eq!(spec.name, "Viewer Grounded Perception v1");
    let names: Vec<&str> = spec.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "search_entity_candidates",
            "search_bilibili_videos",
            "get_bilibili_video",
            "submit_viewer_perception"
        ]
    );
    assert!(spec.tools[3].terminal);

    let spec = audience_agent_spec(&rules());
    assert_eq!(spec.name, "Audience Situation Intelligence");
    let names: Vec<&str> = spec.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "search_bilibili_videos",
            "get_bilibili_video",
            "get_viewer_analysis",
            "query_graph",
            "submit_audience_situation"
        ]
    );
    assert!(spec.tools[4].terminal);
}

/// blocking 客户端的 drop 禁在 async 上下文（M3-B 已证）；测试收尾统一走这里。
async fn spawn_drop<T: Send + 'static>(value: T) {
    tokio::task::spawn_blocking(move || drop(value))
        .await
        .expect("drop task");
}

/// R4：基础设施失败走 Fatal 通道——不白标为模型可修正的校验拒收，
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
