//! M5-D1 单查全链验收钉：POST /api/runs kind=viewer → collect(SingleViewer)
//! → baseline → Grounded Perception → Audience → done，全程 wiremock 双面
//! （Bilibili 17 挂载 + LLM /chat/completions 动态终局响应），零真实外呼。
//!
//! 时序设计：episode/mention/entity id 对「collect 落定后的 viewer 语料」确定性
//! 派生（stable_hash 链路），但 collect 时刻戳未知 → LLM 响应必须惰性构造。
//! 为此 /chat/completions 用单一动态 Respond（condvar cell）：viewer/audience
//! 两类请求各自等主线程填好提交体（最慢 60s）。单一 mount 无 wiremock 级联
//! 探险（同 matcher 双挂 expect(1) 会饿死其一——批 A 复盘的教训）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

use live_core::agent::pipeline::viewer_input_bundle;
use live_core::episodes::baseline::build_factual_baseline;
use live_core::graph::store::mention_id_of;
use live_core::models::MentionSpan;
use live_server::app::{AppState, build_app};

mod common;
use live_server::registry::Registry;

// ---------------------------------------------------------------------------
// wiremock 布景：Bilibili 17 挂载（与 live-core collect_mock mount_baseline 同源）
// ---------------------------------------------------------------------------

fn json_ok(data: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"code": 0, "message": "0", "data": data}))
}

async fn mount_bilibili(server: &MockServer, expect_path: &str, template: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(expect_path))
        .respond_with(template)
        .mount(server)
        .await;
}

async fn mount_bilibili_baseline(server: &MockServer) {
    mount_bilibili(
        server,
        "/x/web-interface/nav",
        json_ok(json!({
            "isLogin": true, "mid": 42, "uname": "me",
            "wbi_img": {
                "img_url": "https://i0.hdslb.com/bfs/wbi/abc123.png",
                "sub_url": "https://i0.hdslb.com/bfs/wbi/def456.png"
            }
        })),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/app-room/v2/guardTab/topListNew",
        json_ok(json!({"top3": [], "list": []})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/relation/stat",
        json_ok(json!({"following": 5, "follower": 9})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/wbi/acc/info",
        json_ok(json!({"name": "昵称", "face": "face", "sign": "签名", "level": 5})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/relation/followings",
        json_ok(json!({"list": [{"mid": 2001, "uname": "关注1", "sign": "s"}]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/wbi/arc/search",
        json_ok(json!({"list": {"vlist": [
            {"aid": 80433022, "bvid": "BV1xx", "title": "投稿1", "description": "d", "created": 1700000000}
        ]}})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/polymer/web-dynamic/v1/feed/space",
        json_ok(json!({"items": [{
            "id_str": "998",
            "modules": {
                "module_author": {"pub_ts": 1700001000},
                "module_dynamic": {"desc": {"text": "动态文本"}}
            }
        }]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/v3/fav/folder/created/list-all",
        json_ok(json!({"list": [{"id": 555, "title": "默认收藏夹", "media_count": 2, "attr": 0}]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/v3/fav/resource/list",
        json_ok(json!({
            "medias": [{
                "bvid": "BV2yy", "title": "收藏1", "intro": "i", "fav_time": 1700002000,
                "upper": {"mid": 3001, "name": "up1"}
            }],
            "has_more": false
        })),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/bangumi/follow/list",
        ResponseTemplate::new(404),
    )
    .await;
    mount_bilibili(
        server,
        "/x/space/lastplaygame/v2",
        json_ok(json!({"list": [{"name": "游戏A", "game_id": 7, "summary": "好玩"}]})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/web-interface/view",
        json_ok(json!({
            "tid": 167, "tname": "知识", "parent_tid": 36, "tname_v2": "科普",
            "title": "T", "pubdate": 1700000000, "owner": {"mid": 3001, "name": "up1"}
        })),
    )
    .await;
    mount_bilibili(
        server,
        "/x/tag/archive/tags",
        json_ok(json!([{"tag_name": "tagA"}, {"name": "tagB"}])),
    )
    .await;
    mount_bilibili(
        server,
        "/x/web-interface/wbi/search/square",
        json_ok(json!({"trending": {"list": [{"keyword": "热词1"}]}})),
    )
    .await;
    mount_bilibili(
        server,
        "/x/v2/reply",
        json_ok(json!({"replies": [{
            "rpid_str": "77",
            "member": {"mid": "8877", "uname": "路人甲"},
            "content": {"message": "前排"},
            "like": 5, "ctime": 1700003000, "rcount": 1
        }]})),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/web-room/v1/record/getList",
        json_ok(json!({"count": 2, "list": [
            {"rid": "R1Ex", "title": "回放A", "area_name": "虚拟主播", "parent_area_name": "娱乐",
             "start_timestamp": 1700000000, "end_timestamp": 1700000200, "danmu_num": 2, "length": 120}
        ]})),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/web-room/v1/record/getInfoByLiveRecord",
        json_ok(
            json!({"live_record_info": {"rid": "R1Ex"}, "dm_info": {"num": 1, "total_num": 2}}),
        ),
    )
    .await;
    mount_bilibili(
        server,
        "/xlive/web-room/v1/dM/getDMMsgByPlayBackID",
        json_ok(json!({"dm": {"dm_info": [
            {"text": "弹幕一", "uid": 998877, "medal": {"medal_name": "牌子", "medal_level": 7}},
            {"text": "弹幕二", "uid": 998878, "medal": null}
        ]}})),
    )
    .await;
}

// ---------------------------------------------------------------------------
// LLM 动态终局响应（condvar cell 惰性注入）
// ---------------------------------------------------------------------------

type Cell = Arc<(Mutex<HashMap<&'static str, Value>>, Condvar)>;

struct DynamicSubmit {
    cell: Cell,
    model: String,
    /// 剧本化失败面（partial=true e2e 钉）：audience 终局恒 500，不等 cell。
    fail_audience: bool,
}

impl Respond for DynamicSubmit {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(json!({}));
        let content = body["messages"]
            .as_array()
            .and_then(|m| m.get(1))
            .and_then(|m| m["content"].as_str());
        let (kind, tool): (&'static str, &'static str) = match content {
            Some(c) if c.starts_with("对下面完整Episode") => {
                ("viewer", "submit_viewer_perception")
            }
            Some(c) if c.starts_with("基于下面的全员索引") => {
                ("audience", "submit_audience_situation")
            }
            _ => {
                return ResponseTemplate::new(500)
                    .set_body_string(format!("动态响应未识别的请求面：{content:?}"));
            }
        };
        if self.fail_audience && kind == "audience" {
            return ResponseTemplate::new(500)
                .set_body_string("audience 终局剧本化失败（partial=true 钉脚）");
        }
        let (lock, cond) = &*self.cell;
        // 先验后等：cell 常由主线程先行填满，先 wait 会把「无 waiter 的 notify」
        // 吞掉并白耗整段 timeout（已咬过一次：首回合 60s×2 拖跨测试时限）。
        let mut guard = lock.lock().expect("cell poisoned");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !guard.contains_key(kind) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (g, _) = cond
                .wait_timeout(guard, deadline - now)
                .expect("cell notified");
            guard = g;
        }
        let Some(submission) = guard.remove(kind) else {
            return ResponseTemplate::new(500).set_body_string(format!("cell 超时：{kind}"));
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "id": format!("chatcmpl-{kind}"),
            "object": "chat.completion",
            "created": 1_800_000_000,
            "model": self.model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": null,
                    "tool_calls": [{
                        "id": format!("call-{kind}-1"),
                        "type": "function",
                        "function": {"name": tool, "arguments": json!({"submission": submission}).to_string()},
                    }],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
        }))
    }
}

fn fill(cell: &Cell, kind: &'static str, submission: Value) {
    cell.0
        .lock()
        .expect("cell poisoned")
        .insert(kind, submission);
    cell.1.notify_all();
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn wait_until(deadline: Duration, mut probe: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if probe() {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

async fn oneshot(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body
        .map(|value| axum::body::Body::from(serde_json::to_vec(&value).unwrap()))
        .unwrap_or_else(axum::body::Body::empty);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// 主验收：全链 walk
// ---------------------------------------------------------------------------

/// 从 collect 落定后的 out_dir 惰性推导 viewer/audience 双终局提交体
/// （episode/mention/entity id 与 live-core mint 公式同一算法）。
/// 返回 (viewer_submission, audience_submission)。
/// SingleViewer("1003") scoped 直采公共件（删码专项 ID-4：原三处逐字同形）。
/// W2-C1 纪律载体：blocking client 绝不能在 async ctx 直调——scoped 线程卸出去。
fn collect_single_viewer_once(host: &str, config: &live_core::config::Config) {
    let host = host.to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let client = live_core::bilibili::BilibiliClient::with_origin(
                    &host,
                    &host,
                    &config.bilibili.cookie,
                    config.collection.request_delay_seconds,
                    config.collection.timeout_seconds,
                )
                .expect("client builds");
                let mut emit_fn = |_: &str| {};
                live_core::collector::run::collect_with_client(
                    client,
                    config,
                    live_core::collector::run::CollectMode::SingleViewer("1003".to_string()),
                    &mut emit_fn,
                )
                .expect("collect completes");
            })
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
    });
}

fn derive_submissions(out_dir: &Path) -> (Value, Value) {
    let analysis = build_factual_baseline(out_dir, 1000).expect("baseline");
    let raw = read(&out_dir.join("viewers").join("1003.json"));
    let profile = analysis["viewer_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["viewer"]["id"] == "1003")
        .unwrap()
        .clone();
    let bundle = viewer_input_bundle(
        &raw,
        &profile,
        "m5d-single",
        "chat_completions",
        &json!({"enabled": false, "effort": "high", "replay_content": true}),
        &["取向优先".to_string()],
        1000,
    );
    let episode_id = bundle.episodes[0].episode_id.clone();
    let field_text = bundle.episodes[0]
        .fields
        .iter()
        .find(|f| f.path == "title")
        .map(|f| f.text.clone())
        .expect("episode 必然携带 title 字段");
    // 「收藏1」全字段码点区间（title 原文恒等于字段全长）
    let span = MentionSpan {
        mention_id: "m1".to_string(),
        episode_id: episode_id.clone(),
        field_path: "title".to_string(),
        text: field_text.clone(),
        start: 0,
        end: field_text.chars().count() as i64,
        mention_type: "作品名".to_string(),
        origin: "explicit".to_string(),
        proposed_entity_name: String::new(),
        proposed_entity_type: String::new(),
        entity_ref: "entity:e1".to_string(),
        confidence: 0.9,
    };
    let mention_id = mention_id_of("1003", &span);
    // 与 live-core apply_resolution 同一公式（viewer_id, entity_type, 排好序的 grounding）
    let grounding = vec![mention_id.clone()];
    let entity_id = format!(
        "entity:{}:{}",
        live_core::episodes::safe_type("游戏"),
        live_core::episodes::hash_parts(
            &[
                "1003".to_string(),
                "游戏".to_string(),
                live_core::episodes::py_repr_list(&grounding),
                String::new(),
            ],
            18,
        ),
    );
    let viewer_submission = json!({
        "viewer_id": "1003",
        "profile_summary": "单查验收：公开收藏出现演示作品，形成可追溯证据链。",
        "mentions": [{
            "mention_id": "m1", "episode_id": episode_id, "field_path": "title",
            "text": span.text, "start": span.start, "end": span.end,
            "mention_type": "作品名", "origin": "explicit",
            "proposed_entity_name": "演示作品", "proposed_entity_type": "游戏",
            "entity_ref": "entity:e1", "confidence": 0.9
        }],
        "entities": [{
            "local_id": "e1", "canonical_name": "演示作品", "entity_type": "游戏",
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
    });
    let audience_submission = json!({
        "executive_summary": "单查验收：一名观众的公开收藏围绕演示作品形成初步信号。",
        "audience_structure": [],
        "interest_graph": [{
            "entity_id": entity_id, "entity": "演示作品", "entity_type": "游戏",
            "parent_entities": [], "angles": [], "viewer_ids": ["1003"],
            "status": "无法判断", "confidence": 0.6,
            "evidence_summary": "单查首轮基线。", "evidence_mention_ids": [mention_id]
        }],
        "communities": [], "situations": [], "content_opportunities": [],
        "individual_highlights": [], "content_calendar": [],
        "data_gaps": [], "safety_notes": [], "leads": []
    });
    (viewer_submission, audience_submission)
}

#[tokio::test(flavor = "multi_thread")]
async fn single_viewer_run_walks_whole_chain_to_done() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    let llm = MockServer::start().await;
    let cell: Cell = Arc::new((Mutex::new(HashMap::new()), Condvar::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(DynamicSubmit {
            cell: cell.clone(),
            model: "m5d-single".to_string(),
            fail_audience: false,
        })
        .mount(&llm)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let out_dir: PathBuf = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let yaml = common::yaml_template(
        None,
        "m5d-single",
        "SESSDATA=test",
        "test-key",
        "LLM_URI",
        "m5d-single",
    )
    .replace(
        "OUTPUT_DIR",
        &out_dir.display().to_string().replace('\\', "/"),
    )
    .replace("LLM_URI", &llm.uri());
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, yaml).unwrap();

    let registry = Registry::new();
    let app = build_app(AppState {
        config_path: config_path.clone(),
        web_root: tmp.path().join("no-dist"),
        registry: registry.clone(),
        demo: false,
        data_root: None,
        bilibili_hosts: Some((bilibili.uri(), bilibili.uri())),
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });

    // 触发：kind=viewer → CollectMode::SingleViewer
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "viewer", "viewer_uid": "1003"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();

    // 门①：collect 落定（collection.json 终写 complete）
    let collection_path = out_dir.join("collection.json");
    let collected = wait_until(Duration::from_secs(30), || {
        std::fs::read_to_string(&collection_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|c| c["status"] == "complete")
    });
    assert!(collected, "collect 未在 30s 内落定");
    let viewer_file = out_dir.join("viewers").join("1003.json");
    assert!(viewer_file.exists(), "单查应只落 viewers/1003.json");

    // 门②：惰性构造两个终局提交体（确定性 id 链）
    let (viewer_submission, audience_submission) = derive_submissions(&out_dir);
    fill(&cell, "viewer", viewer_submission);
    fill(&cell, "audience", audience_submission);

    // 门③：run 到终
    let record = registry.get(&run_id).expect("run registered");
    let finished = wait_until(Duration::from_secs(90), || {
        let r = record.lock().expect("record poisoned");
        r.status == "done" || r.status == "failed"
    });
    let snapshot = live_server::registry::run_to_json(&record.lock().expect("record poisoned"));
    if !finished {
        // 诊断面：run 卡在哪一侧——dump LLM 面已收请求的首段正文。
        let llm_seen = llm.received_requests().await.unwrap_or_default();
        let bodies: Vec<String> = llm_seen
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).chars().take(160).collect())
            .collect();
        panic!(
            "run 未在 90s 内到终：{snapshot}\nllm 已收请求数={}\n{bodies:#?}",
            llm_seen.len()
        );
    }
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert_eq!(snapshot["partial"], false, "{snapshot}");
    assert_eq!(snapshot["kind"], "viewer");
    assert_eq!(snapshot["viewer_uid"], "1003");
    assert_eq!(snapshot["outcome"]["status"], "complete", "{snapshot}");
    assert_eq!(snapshot["outcome"]["viewer_count"], 1, "{snapshot}");

    // 器内足迹：stage 机锚点次序（queued→collecting→episodes→per_viewer_ai 的 indirect 证据）
    let events: Vec<&str> = snapshot["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let index_of =
        |needle: &str| -> Option<usize> { events.iter().position(|line| line.contains(needle)) };
    let trig = index_of("触发 kind=viewer").expect("触发足迹");
    let perception = index_of("[AI] Grounded Perception 昵称（1003）").expect("per viewer 足迹");
    let finalize = index_of("状态 → done").expect("finalize 足迹");
    assert!(
        trig < perception && perception < finalize,
        "events 次序错乱：{events:?}"
    );

    // wiremock 双侧请求证真
    let llm_requests = llm.received_requests().await.expect("llm requests");
    assert_eq!(
        llm_requests.len(),
        2,
        "LLM 面应当恰好两次终局提交：viewer + audience"
    );
    let bil_requests = bilibili
        .received_requests()
        .await
        .expect("bilibili requests");
    assert!(
        bil_requests
            .iter()
            .any(|r| r.url.path() == "/x/web-interface/nav"),
        "必须经 Bilibili 登录态面"
    );

    // 门④：数据端点回读（tree/graph/viewers 全线接通）
    let (status, viewers) = oneshot(&app, "GET", "/api/rooms/983/viewers", None).await;
    assert_eq!(status, 200, "{viewers}");
    let row = viewers
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["uid"] == "1003")
        .expect("单查观众入列");
    assert_eq!(row["ai_status"], "complete");
    assert_eq!(row["ai_completed"], true);

    let (status, tree) = oneshot(&app, "GET", "/api/rooms/983/viewers/1003/tree", None).await;
    assert_eq!(status, 200, "{tree}");
    let mentions = tree["mentions"].as_array().unwrap();
    assert!(!mentions.is_empty(), "单查观众应有 mention 入库：{tree}");
    assert!(
        mentions.iter().any(|m| m["canonical_name"] == "演示作品"),
        "mention 应归属 NEW_ENTITY 解析出的实体：{mentions:?}"
    );

    let (status, graph) = oneshot(&app, "GET", "/api/rooms/983/viewers/1003/graph", None).await;
    assert_eq!(status, 200, "{graph}");
    assert!(
        !graph["elements"].as_array().unwrap().is_empty(),
        "局部图应非空：{graph}"
    );
}

// ---------------------------------------------------------------------------
// Z4 动作平面 e2e：collect_streamer 事实层终局 / ai_viewers 停点 / ai_audience 续跑
// ---------------------------------------------------------------------------

fn build_zip_fixture(
    bilibili_uri: &str,
    llm_uri: Option<&str>,
) -> (tempfile::TempDir, PathBuf, axum::Router, Registry) {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir: PathBuf = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let yaml = common::yaml_template(
        None,
        "m5d-staged",
        "SESSDATA=test",
        "test-key",
        llm_uri.unwrap_or("http://127.0.0.1:9/v1"),
        "m5d-staged",
    )
    .replace(
        "OUTPUT_DIR",
        &out_dir.display().to_string().replace('\\', "/"),
    );
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    let registry = Registry::new();
    let app = build_app(AppState {
        config_path: config_path.clone(),
        web_root: tmp.path().join("no-dist"),
        registry: registry.clone(),
        demo: false,
        data_root: None,
        bilibili_hosts: Some((bilibili_uri.to_string(), bilibili_uri.to_string())),
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });
    (tmp, config_path, app, registry)
}

fn run_terminal(run_id: &str, registry: &Registry, timeout: Duration) -> Value {
    let record = registry.get(run_id).expect("run registered");
    let finished = wait_until(timeout, || {
        let r = record.lock().expect("record poisoned");
        r.status == "done" || r.status == "failed"
    });
    let snapshot = live_server::registry::run_to_json(&record.lock().expect("record poisoned"));
    assert!(finished, "run 未在 {:?} 内到终：{snapshot}", timeout);
    snapshot
}

/// Z4a：collect_streamer = 事实层终局——主播产物在位、观众面不生、AI 面未涉、无需 LLM。
#[tokio::test(flavor = "multi_thread")]
async fn staged_collect_streamer_is_facts_only_terminal() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    let (_tmp, config_path, app, registry) = build_zip_fixture(&bilibili.uri(), None);
    let out_dir = live_core::config::load_config(&config_path)
        .expect("config loads")
        .output_dir;

    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "collect_streamer"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(60));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert_eq!(snapshot["kind"], "collect_streamer");

    // 主播事实面在位；观众面/AI 面都不该生（动作语义=只采主播）。
    assert!(
        out_dir.join("streamer.json").exists(),
        "主播 profile 面板应落盘"
    );
    assert_eq!(read(&out_dir.join("collection.json"))["status"], "complete");
    let viewers_dir = out_dir.join("viewers");
    assert!(
        !viewers_dir.exists()
            || std::fs::read_dir(&viewers_dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "collect_streamer 不得写 viewers/：{viewers_dir:?}"
    );
    assert!(
        !out_dir.join("ai/state.json").exists(),
        "collect_streamer 不得涉 AI 状态面"
    );

    // P0-4（复盘解耦）：事实层终局即 T0 出卡——四个数是语料纯规则，零 AI；
    // 命名仍属认知层（缺位 null），出卡足迹进 events。
    let recap = read(&out_dir.join("ai").join("recap.json"));
    assert!(
        matches!(recap["status"].as_str(), Some("ready") | Some("empty")),
        "复盘卡 status 必须是枚举字面：{recap}"
    );
    assert!(recap["naming"].is_null(), "事实层终局绝不伪造 AI 命名");
    assert!(recap["unknown"].is_array(), "未知行恒在（验收钉④）");
    assert!(
        snapshot["events"]
            .as_array()
            .expect("events list")
            .iter()
            .any(|row| row
                .as_str()
                .is_some_and(|text| text.contains("[RECAP] 复盘卡落盘"))),
        "复盘卡落盘足迹应在 events 里：{snapshot}"
    );
}

/// Z4b/c：同一份采集面先 ai_viewers（收口到 viewer 阶段停点、态势不动），
/// 再 ai_audience（观众哈希命中短路过，平稳入 audience 落定态势）。
#[tokio::test(flavor = "multi_thread")]
async fn staged_ai_viewers_stops_then_ai_audience_completes_situation() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    let llm = MockServer::start().await;
    let cell: Cell = Arc::new((Mutex::new(HashMap::new()), Condvar::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(DynamicSubmit {
            cell: cell.clone(),
            model: "m5d-staged".to_string(),
            fail_audience: false,
        })
        .mount(&llm)
        .await;
    let (_tmp, config_path, app, registry) = build_zip_fixture(&bilibili.uri(), Some(&llm.uri()));
    let out_dir = live_core::config::load_config(&config_path)
        .expect("config loads")
        .output_dir;

    // 布景：不经服务端，直接同模式落一次采集面（SingleViewer=1003——本布景 yaml 无
    // additional_viewer_ids，Guards 空名单会拒采；单查种子自带 1003 即够生成采集面）。
    // 纪律（W2-C1 同刃）：blocking client 绝不能在 async ctx 直调——scoped 线程卸出去。
    let config = live_core::config::load_config(&config_path).expect("config loads");
    collect_single_viewer_once(&bilibili.uri(), &config);
    assert!(
        out_dir.join("viewers").join("1003.json").exists(),
        "布景采集应落 viewers/1003.json"
    );

    // 一颗大头：先填 viewer 提交体——ai_viewers 跑通后）钉停点。
    let (viewer_submission, audience_submission) = derive_submissions(&out_dir);
    fill(&cell, "viewer", viewer_submission);
    fill(&cell, "audience", audience_submission);

    // —— ai_viewers：跑到 viewer 阶段收口，audience 不启 ——
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "ai_viewers"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(90));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert_eq!(
        snapshot["outcome"]["stage_terminal"], "per_viewer_ai",
        "{snapshot}"
    );
    let state_path = out_dir.join("ai/state.json");
    let state = read(&state_path);
    assert_eq!(state["status"], "complete");
    assert_eq!(state["stage_terminal"], "per_viewer_ai", "{state}");
    assert!(
        !out_dir.join("ai/situation.json").exists(),
        "ai_viewers 不得产态势面（situation 未动）"
    );

    // —— ai_audience：viewer 哈希命中短路，走 audience 落定 ——
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "ai_audience"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(90));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert!(
        out_dir.join("ai/situation.json").exists(),
        "ai_audience 必须落定态势面"
    );
}

// ---------------------------------------------------------------------------
// Z5 重采保 AI 全链钉
// ---------------------------------------------------------------------------

/// Z5 前置真因钉（引擎层、零 LLM）：同一 mock 两轮同模式采集 → per-viewer
/// input_hash 必须逐字节稳定（complete_cache 跨采集复用的物理前提）。
/// 不稳 → 直接打印 input_payload 顶键 diff，当场出真凶。
#[tokio::test(flavor = "multi_thread")]
async fn recollect_same_data_keeps_viewer_input_hash_stable() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    let (_tmp, config_path, _app, _registry) = build_zip_fixture(&bilibili.uri(), None);
    let config = live_core::config::load_config(&config_path).expect("config loads");
    let out_dir = config.output_dir.clone();
    let bilibili_host = bilibili.uri();
    // 与 pipeline 运行期同向的输入面（settings 两轮同参，横比只盯采集面漂移）。
    let bundle_of = |out_dir: &Path| -> (String, Value) {
        let analysis = build_factual_baseline(out_dir, 1000).expect("baseline");
        let raw = read(&out_dir.join("viewers").join("1003.json"));
        let profile = analysis["viewer_profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["viewer"]["id"] == "1003")
            .unwrap()
            .clone();
        let bundle = live_core::agent::pipeline::viewer_input_bundle(
            &raw,
            &profile,
            "m5d-staged",
            "chat_completions",
            &json!({"enabled": false, "effort": "high", "replay_content": true}),
            &["取向优先".to_string()],
            1000,
        );
        (bundle.input_hash.clone(), bundle.input_payload.clone())
    };
    collect_single_viewer_once(&bilibili_host, &config);
    let (hash_round1, payload_round1) = bundle_of(&out_dir);
    collect_single_viewer_once(&bilibili_host, &config);
    let (hash_round2, payload_round2) = bundle_of(&out_dir);
    if hash_round1 != hash_round2 {
        for key in payload_round1.as_object().map(|o| o.keys()).unwrap() {
            let (a, b) = (&payload_round1[key], &payload_round2[key]);
            if a != b {
                eprintln!("[hash-diff] key={key}\n  round1={a}\n  round2={b}");
            }
        }
    }
    assert_eq!(
        hash_round1, hash_round2,
        "同数据重采必须产出稳定 input_hash（漂移键见 stderr [hash-diff]）"
    );
}

/// 单查采集 → ai_viewers 落定缓存 → 同数据重采（同模式再跑一遍 SingleViewer）→
/// ai/ 现场实体零碾平 → 第二轮 ai_viewers 完全复用：LLM 请求数零新增（事实未变 →
/// input_hash 相等 → complete_cache 命中——重采的 AI 成本下限 = 0）。
/// 布景纪律：两轮采集同模式同种子（manual/1003 两轮一致）——seed_source 与种子件
/// 若跨姿势变更（manual↔guard）属事实面变化，viewers 索引位重判为正确语义，本钉不混。
#[tokio::test(flavor = "multi_thread")]
async fn staged_recollect_keeps_ai_cache_and_second_ai_run_reuses_zero_llm() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    let llm = MockServer::start().await;
    let cell: Cell = Arc::new((Mutex::new(HashMap::new()), Condvar::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(DynamicSubmit {
            cell: cell.clone(),
            model: "m5d-z5".to_string(),
            fail_audience: false,
        })
        .mount(&llm)
        .await;
    let (_tmp, config_path, app, registry) = build_zip_fixture(&bilibili.uri(), Some(&llm.uri()));
    let config = live_core::config::load_config(&config_path).expect("config loads");
    let out_dir = config.output_dir.clone();

    let bilibili_host = bilibili.uri();

    // ── 第一轮采集 ──
    collect_single_viewer_once(&bilibili_host, &config);
    assert!(
        out_dir.join("viewers").join("1003.json").exists(),
        "第一轮采集应落 viewers/1003.json"
    );

    // ── ai_viewers #1：1 次 viewer 提交落定缓存 ──
    let (viewer_submission, audience_submission) = derive_submissions(&out_dir);
    fill(&cell, "viewer", viewer_submission);
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "ai_viewers"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(90));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    let llm_calls_round1 = llm.received_requests().await.expect("llm requests").len();
    assert_eq!(
        llm_calls_round1, 1,
        "首轮 ai_viewers 应恰好 1 次 viewer 提交"
    );

    // ── 同数据重采（Z5 主钉）：ai/ 现场实体零碾平 ──
    collect_single_viewer_once(&bilibili_host, &config);
    assert!(
        out_dir.join("ai/state.json").exists(),
        "Z5 重采保 AI：重采后 ai/state.json 必须原地保活"
    );
    assert!(
        out_dir
            .join("ai")
            .join("perception")
            .join("viewers")
            .join("1003.json")
            .exists(),
        "Z5 重采保 AI：重采后观众缓存 ai/perception/viewers/1003.json 必须原地保活"
    );
    assert!(
        out_dir.join("viewers").join("1003.json").exists(),
        "重采后事实面必须重建（viewers/1003.json 在场）"
    );

    // ── ai_viewers #2：事实未变 → input_hash 命中 → LLM 零新增 ──
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "ai_viewers"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(90));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    let llm_calls_round2 = llm.received_requests().await.expect("llm requests").len();
    assert_eq!(
        llm_calls_round2, llm_calls_round1,
        "Z5 重采保 AI：同数据重采后再跑 ai_viewers 必须零新增 LLM（complete_cache 复用）"
    );

    // ── ai_audience #1：viewer 复用 + 1 次 audience 提交落定态势 ──
    fill(&cell, "audience", audience_submission);
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "ai_audience"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(90));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert!(
        out_dir.join("ai/situation.json").exists(),
        "ai_audience 必须落定态势面"
    );
    let llm_calls_audience1 = llm.received_requests().await.expect("llm requests").len();
    assert_eq!(llm_calls_audience1, 2, "viewer + audience 各一次提交");

    // ── 第三轮：重采 → ai_audience #2 零新增（态势哈希也跨采集稳定）──
    collect_single_viewer_once(&bilibili_host, &config);
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "ai_audience"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(90));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    let llm_calls_audience2 = llm.received_requests().await.expect("llm requests").len();
    assert_eq!(
        llm_calls_audience2, llm_calls_audience1,
        "Z5 重采保 AI：同数据重采后再跑 ai_audience 必须零新增 LLM（态势哈希跨采集稳定）"
    );
}

// ---------------------------------------------------------------------------
// partial=true e2e（kickoff M6-B 挂账消化 2）：audience 期失败 → failed(partial)
// ---------------------------------------------------------------------------

/// viewer 阶段完整走完 + audience 终局 500 → finalize(failed, partial=true)，
/// 且 ai/state.json 契约键 viewer_stage_status=complete 现场可查（ag3-F1 的端到端面）。
#[tokio::test(flavor = "multi_thread")]
async fn single_viewer_run_audience_failure_marks_partial_true() {
    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    let llm = MockServer::start().await;
    let cell: Cell = Arc::new((Mutex::new(HashMap::new()), Condvar::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(DynamicSubmit {
            cell: cell.clone(),
            model: "m5d-single".to_string(),
            fail_audience: true,
        })
        .mount(&llm)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let out_dir: PathBuf = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let yaml = common::yaml_template(
        None,
        "m5d-single",
        "SESSDATA=test",
        "test-key",
        "LLM_URI",
        "m5d-single",
    )
    .replace(
        "OUTPUT_DIR",
        &out_dir.display().to_string().replace('\\', "/"),
    )
    .replace("LLM_URI", &llm.uri());
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, yaml).unwrap();

    let registry = Registry::new();
    let app = build_app(AppState {
        config_path: config_path.clone(),
        web_root: tmp.path().join("no-dist"),
        registry: registry.clone(),
        demo: false,
        data_root: None,
        bilibili_hosts: Some((bilibili.uri(), bilibili.uri())),
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });

    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "viewer", "viewer_uid": "1003"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();

    // collect 落定后 fill viewer 终局（audience 故意不填——fail_audience 使其无用）。
    let collection_path = out_dir.join("collection.json");
    let collected = wait_until(Duration::from_secs(30), || {
        std::fs::read_to_string(&collection_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|c| c["status"] == "complete")
    });
    assert!(collected, "collect 未在 30s 内落定");
    let (viewer_submission, _audience) = derive_submissions(&out_dir);
    fill(&cell, "viewer", viewer_submission);

    let record = registry.get(&run_id).expect("run registered");
    let finished = wait_until(Duration::from_secs(90), || {
        let r = record.lock().expect("record poisoned");
        r.status == "done" || r.status == "failed"
    });
    let snapshot = live_server::registry::run_to_json(&record.lock().expect("record poisoned"));
    assert!(finished, "run 未在 90s 内到终：{snapshot}");
    assert_eq!(snapshot["status"], "failed", "{snapshot}");
    assert_eq!(
        snapshot["partial"], true,
        "viewer 阶段已完成 + audience 失败必须如实 partial=true：{snapshot}"
    );
    assert!(
        snapshot["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap_or("").contains("状态 → failed")),
        "events 须带 failed 足迹：{snapshot}"
    );
    // 契约键现场：fail_run_and_state 必须写 viewer_stage_status=complete。
    let state = read(&out_dir.join("ai").join("state.json"));
    assert_eq!(
        state["viewer_stage_status"], "complete",
        "pipeline 契约键现场：{state}"
    );
    assert_eq!(state["status"], "failed", "{state}");
}

// ---------------------------------------------------------------------------
// G2 冒烟：线索→采集→对账 两轮（M4.x 账本消费环端到端）
// ---------------------------------------------------------------------------

/// G2 立项裁决书§1 冒烟主戏：两轮「线索→采集→对账」自动化核对。
///
/// 轮一（线索入账）：Guards 动作面采集成面 → ai_audience 经 wiremock LLM
/// 终局带回 ≥1 条 audience lead → 账本出现 pending_approval 行（以
/// AUDIENCE_VIEWER_ID 入账），/rooms/:uid/overview 读数面同步呈现 pending。
/// 审批 seam（G2-B 已端点化）：POST /api/rooms/:uid/leads/:lead_id/approve
/// 翻转 pending → approved（幂等重放同终态不写账本）；旧红线（手工编辑
/// leads.jsonl）仍是合法的旁路面。
/// 轮二（采集消费+对账）：撬开 lead_fetch_budget_per_run=1 → 同模式非单查 collect
/// 尾段按预算消费该 creator lead，wiremock /x/space/wbi/arc/search 以 mid=3001
/// 命中并返还 1 条投稿 → 行转 consumed、yield_count>0、collection.json
/// complete + leads_consumed=1、viewers/1003.json 重建；同时 ai/ 缓存与账本
/// 跨轮存续（Z5：reset_output 白名单外，屠刀不碰），LLM 面零新增（事实未变→
/// 哈希命中、缓存全复用）。
#[tokio::test(flavor = "multi_thread")]
async fn g2_smoke_two_rounds_lead_to_collect_to_recon() {
    use live_core::leads::LeadStatus;
    // G2 表形态（design §9.2 行 254）：读取面唯一源 = discovery_leads 表。
    let ledger_rows = |out_dir: &std::path::Path| -> Vec<live_core::leads::LedgerRow> {
        let store = live_core::graph::store::Store::open(&out_dir.join("graph/perception.sqlite3"))
            .expect("store opens");
        live_core::leads::read_rows(&store).expect("rows read")
    };

    let bilibili = MockServer::start().await;
    mount_bilibili_baseline(&bilibili).await;
    let llm = MockServer::start().await;
    let cell: Cell = Arc::new((Mutex::new(HashMap::new()), Condvar::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(DynamicSubmit {
            cell: cell.clone(),
            model: "g2-smoke".to_string(),
            fail_audience: false,
        })
        .mount(&llm)
        .await;
    let (_tmp, config_path, app, registry) = build_zip_fixture(&bilibili.uri(), Some(&llm.uri()));
    // 给 Guards 采集面一个附加观众种子（yaml_template 默认空名单）。
    let yaml = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(
        &config_path,
        yaml.replace(
            "additional_viewer_ids: []",
            "additional_viewer_ids: [\"1003\"]",
        ),
    )
    .unwrap();
    let out_dir = live_core::config::load_config(&config_path)
        .expect("config loads")
        .output_dir;

    // ── 轮一①：Guards 采集成面 ──
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "collect_guards"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(60));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert_eq!(snapshot["kind"], "collect_guards");
    assert!(
        out_dir.join("viewers").join("1003.json").exists(),
        "采集面应落 viewers/1003.json"
    );
    // 默认预算 0 → 本轮不得面世 leads_consumed（M4.x-T1 冻结）。
    let first_collection = read(&out_dir.join("collection.json"));
    assert_eq!(first_collection["status"], "complete");
    assert!(
        first_collection.get("leads_consumed").is_none(),
        "预算 0 不得面世 leads_consumed：{first_collection}"
    );

    // ── 轮一②：ai_audience 终局带回 ≥1 条 audience lead ──
    let (viewer_submission, mut audience_submission) = derive_submissions(&out_dir);
    audience_submission["leads"] = json!([{
        "type": "creator", "locator": "3001",
        "motivation": "G2 冒烟：audience 终局回传一条可消费线索。",
        "expected_signal": "目标 up 的 arc/search 列表返回 ≥1 条投稿。",
        "priority": "medium", "evidence_ids": []
    }]);
    fill(&cell, "viewer", viewer_submission);
    fill(&cell, "audience", audience_submission);
    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "ai_audience"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(90));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert_eq!(snapshot["partial"], false, "{snapshot}");

    // 账本出现 1 条 pending_approval（audience 侧、creator/3001）。
    let rows = ledger_rows(&out_dir);
    assert_eq!(rows.len(), 1, "audience lead 应恰好 1 条账本行：{rows:?}");
    assert_eq!(rows[0].lead_type, "creator");
    assert_eq!(rows[0].locator, "3001");
    assert_eq!(rows[0].status, LeadStatus::PendingApproval, "{rows:?}");
    assert_eq!(
        rows[0].viewer_id, "audience",
        "audience lead 以 AUDIENCE_VIEWER_ID 入账：{rows:?}"
    );
    // 服务端读取侧同步呈现 pending 明细（人工审批工作队列就位）。
    let (status, overview) = oneshot(&app, "GET", "/api/rooms/983/overview", None).await;
    assert_eq!(status, 200, "{overview}");
    assert_eq!(
        overview["leads"]["totals"]["pending_approval"], 1,
        "{overview}"
    );
    assert_eq!(overview["ai"]["status"], "complete", "{overview}");

    // ── 审批（G2-B 审批缝，原 seam 红线已端点化）：
    // POST /api/rooms/:uid/leads/:lead_id/approve 翻转 pending → approved；
    // 幂等重放同终态、表行逐字段不动 ──
    let approve_path = format!("/api/rooms/983/leads/{}/approve", rows[0].dedupe_key);
    let (status, body) = oneshot(&app, "POST", &approve_path, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "approved", "{body}");
    assert_eq!(body["changed"], true, "{body}");
    let settled_ledger = ledger_rows(&out_dir);
    let (status, replay) = oneshot(&app, "POST", &approve_path, None).await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["status"], "approved", "重放返回相同终态：{replay}");
    assert_eq!(replay["changed"], false, "{replay}");
    assert_eq!(ledger_rows(&out_dir), settled_ledger, "幂等重放不得写账本");
    assert_eq!(
        settled_ledger[0].status,
        LeadStatus::Approved,
        "审批缝翻转已落账本"
    );

    // Z5 前置快照：ai/ 缓存字节（重采后必须逐字节不变）。
    let ai_state_before = std::fs::read(out_dir.join("ai/state.json")).unwrap();
    let ai_situation_before = std::fs::read(out_dir.join("ai/situation.json")).unwrap();

    // ── 轮二：撬开 lead_fetch_budget_per_run=1，非单查采集尾段消费 ──
    let yaml = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(
        &config_path,
        yaml.replacen(
            "  timeout_seconds: 5",
            "  timeout_seconds: 5\n  lead_fetch_budget_per_run: 1",
            1,
        ),
    )
    .unwrap();

    let (status, body) = oneshot(
        &app,
        "POST",
        "/api/runs",
        Some(json!({"kind": "collect_guards"})),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run_id = body["run_id"].as_str().unwrap().to_string();
    let snapshot = run_terminal(&run_id, &registry, Duration::from_secs(60));
    assert_eq!(snapshot["status"], "done", "{snapshot}");
    assert_eq!(snapshot["kind"], "collect_guards");
    assert_eq!(snapshot["outcome"]["status"], "complete", "{snapshot}");

    // 对账①：creator 行 consumed + yield_count>0。
    let rows = ledger_rows(&out_dir);
    let consumed = rows
        .iter()
        .find(|r| r.lead_type == "creator" && r.locator == "3001")
        .expect("creator 行必须存在");
    assert_eq!(consumed.status, LeadStatus::Consumed, "{consumed:?}");
    assert!(
        consumed.yield_count > 0,
        "creator 消费应带回 yield：{consumed:?}"
    );
    assert!(consumed.resolution_note.is_empty(), "{consumed:?}");

    // 对账②：collection.json complete + leads_consumed=1。
    let collection = read(&out_dir.join("collection.json"));
    assert_eq!(collection["status"], "complete", "{collection}");
    assert_eq!(collection["leads_consumed"], 1, "{collection}");

    // 对账③：目标观众事实面重建。
    assert!(
        out_dir.join("viewers").join("1003.json").exists(),
        "轮二采集应重建 viewers/1003.json"
    );

    // Z5：重采保 AI——ai/ 缓存字节级原样（reset_output 白名单外）。
    assert_eq!(
        std::fs::read(out_dir.join("ai/state.json")).unwrap(),
        ai_state_before,
        "重采不得碾平 ai/state.json"
    );
    assert_eq!(
        std::fs::read(out_dir.join("ai/situation.json")).unwrap(),
        ai_situation_before,
        "重采不得碾平 ai/situation.json"
    );

    // 消费的真实网络面：creator 3001 必须以 mid=3001 命中 arc/search mock。
    let bil = bilibili
        .received_requests()
        .await
        .expect("bilibili requests");
    assert!(
        bil.iter().any(|r| {
            r.url.path() == "/x/space/wbi/arc/search"
                && r.url
                    .query_pairs()
                    .any(|(key, value)| key == "mid" && value == "3001")
        }),
        "creator 消费必须携带 mid=3001 命中 arc/search：\n{}",
        bil.iter()
            .map(|r| format!("{} {}", r.method, r.url))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // LLM 面零新增：全程恰 1 次 viewer + 1 次 audience 终局提交。
    assert_eq!(
        llm.received_requests().await.expect("llm requests").len(),
        2,
        "两轮全程不得有第三次 LLM 外呼（缓存全命中）"
    );

    // 终账对账：overview 读取侧呈现 consumed=1、pending 清零。
    let (status, overview) = oneshot(&app, "GET", "/api/rooms/983/overview", None).await;
    assert_eq!(status, 200, "{overview}");
    assert_eq!(overview["leads"]["totals"]["consumed"], 1, "{overview}");
    assert_eq!(
        overview["leads"]["totals"]["pending_approval"], 0,
        "{overview}"
    );
    assert_eq!(overview["collection"]["leads_consumed"], 1, "{overview}");
}
