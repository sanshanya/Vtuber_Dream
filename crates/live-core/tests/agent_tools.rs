//! M3-B 验收：ResearchService（缓存/注册表/快照）+ 5 个调查工具 + graph::query 复出。
//!
//! wiremock 挂签名端点需要 nav 桩（wbi mixin）；图侧用 Store::open(:memory:) 直接种子。

use std::path::Path;

use live_core::agent::tools::{
    AudienceAgentCtx, ResearchService, ViewerAgentCtx, get_bilibili_video_tool,
    get_viewer_analysis_tool, known_search_result_ids, query_graph_tool,
    search_bilibili_videos_tool, search_entity_candidates_tool,
};
use live_core::bilibili::BilibiliClient;
use live_core::episodes::{Episode, EpisodeField};
use live_core::graph::query;
use live_core::graph::store::Store;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn nav_stub() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"code": 0, "data": {"wbi_img": {
        "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
        "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
    }}}))
}

fn mock_client_origin(origin: &str) -> BilibiliClient {
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
        fields: vec![EpisodeField {
            path: "title".to_string(),
            text: title.to_string(),
            kind: "text".to_string(),
        }],
        platform_facts: json!({}),
    }
}

// ---------------------------------------------------------------------------
// ResearchService：钳制 / 白名单回落 / 缓存命中零请求 / 注册表 / 快照 / 跨实例回填
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn search_clamps_cache_hit_zero_request_and_snapshots() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(nav_stub())
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/wbi/search/type"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {"result": [
                {"bvid": "BV1", "title": "<em>异环</em> 实录  \n 多段  空白", "author": "up主甲", "tag": "游戏实录", "play": 1200},
                {"bvid": "BV2", "title": "另一个视频", "author": "up主乙"},
                {"bvid": "BV3", "title": "第三个", "author": "up主丙"}
            ]}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let origin = server.uri();
    let registry = tokio::task::spawn_blocking(move || {
        let tmp = tempfile::tempdir().unwrap();
        let mut research = ResearchService::new(tmp.path(), mock_client_origin(&origin), 20);

        // 超长 query 截断到 500 字符 + 越界 order 回落 totalrank + 越界 limit 钳到 cap。
        let long_query = format!("{}{}", "异".repeat(600), "  ");
        let rows = research
            .search(&long_query, "bogus-order", 5000)
            .expect("search");
        assert_eq!(rows.len(), 3);

        // 归一化：HTML 标签剥离、空白折叠（Python clip = \s+ → " " + strip + 截断）。
        assert_eq!(rows[0]["title"].as_str().unwrap(), "异环 实录 多段 空白");
        assert_eq!(rows[0]["result_id"].as_str().unwrap().len(), 16);
        assert_eq!(
            rows[0]["url"].as_str().unwrap(),
            "https://www.bilibili.com/video/BV1"
        );
        assert_eq!(rows[0]["play"], 1200);

        // 注册表登记 + 快照落盘（修复6 + 修复7）。
        let registry = known_search_result_ids(&research);
        assert_eq!(registry.len(), 3);
        let searches_dir = tmp.path().join("ai/searches");
        for id in &registry {
            let snapshot = searches_dir.join(format!("{id}.json"));
            assert!(snapshot.is_file(), "缺少搜索快照 {snapshot:?}");
        }
        assert!(tmp.path().join("ai/research_cache.json").is_file());

        // 第二次同 key → 缓存命中（nav+search 已 expect(1)，再请求会被 wiremock 拒绝）。
        let again = research
            .search(&long_query, "bogus-order", 5000)
            .expect("cache hit");
        assert_eq!(again.len(), 3);

        // 全新实例：注册表从 research_cache.json 回填（跨运行引用回访的前提，Python __init__ 语义）。
        let reopened = ResearchService::new(tmp.path(), mock_client_origin(&origin), 20);
        assert_eq!(known_search_result_ids(&reopened).len(), 3);
        registry
    })
    .await
    .expect("blocking task");
    assert_eq!(registry.len(), 3);

    // 出网参数断言：截断 + 白名单回落 + 钳制 + 签名字段。
    let requests = server.received_requests().await.expect("captured");
    let search_req = requests
        .iter()
        .find(|r| r.url.path() == "/x/web-interface/wbi/search/type")
        .expect("search request");
    let query_string = search_req.url.query().unwrap_or("");
    assert!(query_string.contains("order=totalrank"), "{query_string}");
    assert!(query_string.contains("page_size=20"), "{query_string}");
    assert!(!query_string.contains("bogus-order"), "{query_string}");
    assert!(
        query_string.contains("w_rid="),
        "必须带 wbi 签名: {query_string}"
    );
    assert!(
        query_string.contains("wts="),
        "必须带签名时间戳: {query_string}"
    );
}

/// 安全批 R1：篡改 cache 中的 result_id 不得进入注册表/快照（目录穿越与伪造 id 拦截）。
#[test]
fn tampered_cache_ids_neither_registered_nor_snapshotted() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("ai");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(
        cache_dir.join("research_cache.json"),
        json!({
            "searches": {"q|totalrank|1": [
                {"result_id": "../../evil", "title": "x"},
                {"result_id": "", "title": "y"},
                {"result_id": "0123456789abcdef", "title": "ok"}
            ]},
            "videos": {}
        })
        .to_string(),
    )
    .unwrap();
    let research = ResearchService::new(tmp.path(), mock_client_origin("http://127.0.0.1:1"), 20);
    let ids: Vec<&str> = research.search_results.keys().map(String::as_str).collect();
    assert_eq!(ids, ["0123456789abcdef"], "只有合法 hex16 id 进入注册表");
    let searches_dir = tmp.path().join("ai").join("searches");
    assert!(
        !searches_dir.exists() || std::fs::read_dir(&searches_dir).unwrap().count() == 0,
        "篡改 id 不产生快照文件"
    );
    assert!(!tmp.path().join("evil.json").exists(), "无目录穿越写入");
}

#[tokio::test(flavor = "multi_thread")]
async fn search_tool_error_surface_returns_error_dict() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(nav_stub())
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/wbi/search/type"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let origin = server.uri();
    let out = tokio::task::spawn_blocking(move || {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ViewerAgentCtx {
            viewer_data: json!({}),
            episodes: Default::default(),
            research: ResearchService::new(tmp.path(), mock_client_origin(&origin), 20),
            store: fixture_store(),
            slot: Default::default(),
        };
        let mut tool = search_bilibili_videos_tool::<ViewerAgentCtx>();
        (tool.handler)(&mut ctx, &json!({"query": "异环", "limit": 5}))
    })
    .await
    .expect("blocking task");
    assert_eq!(out["items"], json!([]));
    assert!(out["error"].as_str().unwrap().contains("http 500"), "{out}");
    assert_eq!(out["query"], "异环");
}

#[tokio::test(flavor = "multi_thread")]
async fn video_detail_cached_and_shaped_and_empty_bvid_costs_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {"title": "异环首测", "desc": "简介", "tid": 4, "tname": "游戏",
                     "parent_tid": 1, "tname_v2": "单机游戏", "pubdate": 1_700_000_000,
                     "owner": {"mid": 42, "name": "up主甲"}, "stat": {"view": 900}}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/tag/archive/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": [{"tag_name": "异环"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let origin = server.uri();
    let (first, second, empty) = tokio::task::spawn_blocking(move || {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ViewerAgentCtx {
            viewer_data: json!({}),
            episodes: Default::default(),
            research: ResearchService::new(tmp.path(), mock_client_origin(&origin), 20),
            store: fixture_store(),
            slot: Default::default(),
        };
        let mut tool = get_bilibili_video_tool::<ViewerAgentCtx>();
        let first = (tool.handler)(&mut ctx, &json!({"bvid": "  BV1xxx  "}));
        // 缓存命中：第二调用零新请求（mocks expect(1) 兜底）。
        let second = (tool.handler)(&mut ctx, &json!({"bvid": "BV1xxx"}));
        // 空 bvid：零请求 + 空对象。
        let empty = (tool.handler)(&mut ctx, &json!({"bvid": "   "}));
        (first, second, empty)
    })
    .await
    .expect("blocking task");

    assert_eq!(first["title"], "异环首测");
    assert_eq!(first["platform_category"]["id"], 4);
    assert_eq!(first["platform_category"]["parent_id"], 1);
    assert_eq!(first["platform_category"]["v2_name"], "单机游戏");
    assert_eq!(first["tags"], json!(["异环"]));
    assert_eq!(first["owner"]["mid"], 42);
    assert_eq!(second, first);
    assert_eq!(empty, json!({}));
}

#[test]
fn entity_candidates_ordered_exact_first_and_type_filtered() {
    let mut ctx_store = fixture_store();
    let conn = &mut ctx_store.conn;
    conn.execute(
        "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
         VALUES(?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            "e-exact", "异环", "异环", "game", "", "platform", "{}", "t0", "t1"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
         VALUES(?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            "e-fuzzy", "异环2", "异环2", "game", "", "ai_semantic", "{}", "t0", "t2"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entities(entity_id,canonical_name,normalized_name,entity_type,description,source_kind,properties_json,first_seen_at,last_seen_at) \
         VALUES(?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            "e-other", "异环工作室", "异环工作室", "creator", "", "platform", "{}", "t0", "t3"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entity_aliases(alias_key,entity_id,alias,source_kind,confidence,created_at) \
         VALUES(?,?,?,?,?,?)",
        rusqlite::params![
            "完美世界新作",
            "e-fuzzy",
            "完美世界新作",
            "ai_semantic",
            0.9,
            "t0"
        ],
    )
    .unwrap();

    let dead = "http://127.0.0.1:1";
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = ViewerAgentCtx {
        viewer_data: json!({}),
        episodes: Default::default(),
        research: ResearchService::new(tmp.path(), mock_client_origin(dead), 20),
        store: ctx_store,
        slot: Default::default(),
    };
    let mut tool = search_entity_candidates_tool::<ViewerAgentCtx>();

    let out = (tool.handler)(&mut ctx, &json!({"query": "异环", "limit": 999}));
    // 无类型过滤：全量命中；精确名排首，其余按 last_seen_at 降序。
    assert_eq!(out["count"], 3, "{out}");
    let ids: Vec<&str> = out["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["entity_id"].as_str())
        .collect();
    assert_eq!(ids, ["e-exact", "e-other", "e-fuzzy"], "{out}");
    // 类型过滤 game → 剔除 creator。
    let game_only = (tool.handler)(&mut ctx, &json!({"query": "异环", "entity_type": "game"}));
    assert_eq!(game_only["count"], 2, "{game_only}");
    // 别名命中。
    let by_alias = (tool.handler)(&mut ctx, &json!({"query": "完美世界新作"}));
    assert_eq!(by_alias["count"], 1, "{by_alias}");
    assert_eq!(by_alias["items"][0]["aliases"], json!(["完美世界新作"]));
    // 空 query 零结果（不报错）。
    let blank = (tool.handler)(&mut ctx, &json!({"query": "  "}));
    assert_eq!(blank["count"], 0);
}

#[test]
fn viewer_analysis_found_not_found_and_episode_attachment() {
    let dead = "http://127.0.0.1:1";
    let tmp = tempfile::tempdir().unwrap();
    let store = fixture_store();
    // seed 一个 episode，viewer_analysis include_episodes → 附带。
    store
        .upsert_episode(&episode("177", "ep-1", "异环开荒"))
        .unwrap();
    let mut viewer_analyses = serde_json::Map::new();
    viewer_analyses.insert("177".to_string(), json!({"profile_summary": "舰长甲"}));
    let mut ctx = AudienceAgentCtx {
        viewer_analyses,
        research: ResearchService::new(tmp.path(), mock_client_origin(dead), 20),
        store,
        graph_run_id: None,
        slot: Default::default(),
    };
    let mut tool = get_viewer_analysis_tool::<AudienceAgentCtx>();

    let missing = (tool.handler)(&mut ctx, &json!({"viewer_id": "999"}));
    assert_eq!(missing["error"], "viewer not found");

    let bare = (tool.handler)(&mut ctx, &json!({"viewer_id": "177"}));
    assert_eq!(bare["profile_summary"], "舰长甲");
    assert!(
        bare.get("episodes").is_none(),
        "默认不附带 episodes: {bare}"
    );

    let with_eps = (tool.handler)(
        &mut ctx,
        &json!({"viewer_id": "177", "include_episodes": true, "episode_limit": 99}),
    );
    let eps = with_eps["episodes"].as_array().expect("episodes array");
    assert_eq!(eps.len(), 1);
    assert_eq!(eps[0]["episode_id"], "ep-1");
}

#[test]
fn query_graph_filters_types_predicates_needle_and_hides_situation() {
    let store = fixture_store();
    store
        .begin_run_fixed("run-1", "2026-08-04T00:00:00+00:00", "model-x")
        .unwrap();
    // nodes：实体 + Situation（默认隐藏阶层）
    store
        .upsert_node(
            "ent:1",
            "Entity",
            "异环",
            &json!({"kind": "game"}),
            "ai_semantic",
            None,
        )
        .unwrap();
    store
        .upsert_node("ent:2", "Entity", "完美世界", &json!({}), "platform", None)
        .unwrap();
    store
        .upsert_node(
            "sit:1",
            "Situation",
            "新游上线",
            &json!({}),
            "ai_semantic",
            None,
        )
        .unwrap();
    store
        .upsert_edge(
            "ent:1",
            "ABOUT",
            "ent:2",
            &json!({"note": "发行方"}),
            "ai_semantic",
            Some(0.8),
            &["m-1".to_string()],
            "run-1",
            None,
        )
        .unwrap();
    store
        .upsert_edge(
            "sit:1",
            "TRIGGERS",
            "ent:1",
            &json!({}),
            "ai_semantic",
            None,
            &[],
            "run-1",
            None,
        )
        .unwrap();
    // Situation 边从 situation 发出 —— run-1 的 situation 节点由 run 边归属可见性规则控制。

    let dead = "http://127.0.0.1:1";
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = AudienceAgentCtx {
        viewer_analyses: serde_json::Map::new(),
        research: ResearchService::new(tmp.path(), mock_client_origin(dead), 20),
        store,
        graph_run_id: None,
        slot: Default::default(),
    };
    let mut tool = query_graph_tool::<AudienceAgentCtx>();

    // 智子网: 无约束默认搜 => Situation 节点必须被排除（NA Situation 层仅 run 语境可见）。
    let all = (tool.handler)(&mut ctx, &json!({"query": ""}));
    let node_types: Vec<&str> = all["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["type"].as_str())
        .collect();
    assert!(
        !node_types.contains(&"Situation"),
        "默认必须隐藏 Situation: {all}"
    );

    // needle 命中节点名 + 关联边回填 + edge 证据字段成形。
    let hit = (tool.handler)(&mut ctx, &json!({"query": "异环"}));
    let node_names: Vec<&str> = hit["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert!(node_names.contains(&"异环"), "{hit}");
    let edges = hit["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1, "异环节点邻接边应当唯一: {hit}");
    assert_eq!(edges[0]["predicate"], "ABOUT");
    assert_eq!(edges[0]["evidence_ids"], json!(["m-1"]));

    // predicate 过滤排除 ABOUT → 空边。
    let no_about = (tool.handler)(
        &mut ctx,
        &json!({"query": "异环", "predicates": ["INTERESTED_IN"]}),
    );
    assert_eq!(no_about["edges"], json!([]), "{no_about}");

    // node_types 过滤。
    let by_type = (tool.handler)(&mut ctx, &json!({"query": "", "node_types": ["Entity"]}));
    let names: Vec<&str> = by_type["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert!(names.contains(&"异环"));
    assert!(names.contains(&"完美世界"));

    // run 语境可见性：给出 run_id 后，该 run 的 Situation 边可见（节点+边）。
    ctx.graph_run_id = Some("run-1".to_string());
    let with_run = (tool.handler)(&mut ctx, &json!({"query": ""}));
    let types: Vec<&str> = with_run["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["type"].as_str())
        .collect();
    assert!(
        types.contains(&"Situation"),
        "run 语境下 situation 节点（该 run 发出活跃边）应可见: {with_run}"
    );

    // mention 证据回填：先补 episode 行满足外键，再直插 mention。
    ctx.store
        .upsert_episode(&episode("177", "ep-x", "异环开荒"))
        .unwrap();
    let store_ref = &ctx.store;
    store_ref.conn.execute(
        "INSERT INTO mentions(mention_id,episode_id,viewer_id,field_path,text,start_offset,end_offset,mention_type,origin,proposed_entity_name,proposed_entity_type,confidence,run_id,created_at) \
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            "m-1", "ep-x", "177", "title", "异环", 0, 2, "work", "explicit",
            "异环", "work", 0.9, "run-1", "2026-08-04T00:00:00+00:00"
        ],
    )
    .unwrap();
    let result = query::query(store_ref, "异环", &[], &[], 500, None).unwrap();
    let mentions = result["mentions"].as_array().unwrap();
    assert_eq!(mentions.len(), 1, "边证据 mention 必须回填: {result}");
    assert_eq!(mentions[0]["mention_id"], "m-1");
}
