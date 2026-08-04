//! collect() 编排的 wiremock 集成测试（design M2-B2 验收点）：
//! - happy path：2 大航海 + 1 手工观众，全部源走完，文件面齐全，TAG/分区回灌。
//! - 单源失败隔离：bangumi 404 → status=error，其余照常（失败只影响当前单元）。
//! - 预算耗尽：budget=3 → videos/dynamics/favorites/bangumi/games 全部 budget_skipped。
//!
//! 客户端延迟=0；根地址指向 MockServer。

use std::path::Path;

use live_core::bilibili::BilibiliClient;
use live_core::collector::{CollectError, CollectMode, collect_with_client};
use live_core::config::{
    AgentRuntimeConfig, AiConfig, BilibiliConfig, CollectionConfig, Config, PeerDiscoveryConfig,
    PerceptionConfig, ReasoningConfig,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn json_ok(data: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"code": 0, "message": "0", "data": data}))
}

async fn mount(server: &MockServer, expect_path: &str, template: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(expect_path))
        .respond_with(template)
        .mount(server)
        .await;
}

async fn mount_baseline(server: &MockServer) {
    // nav：登录态 + WBI 键（profile/videos/hot_searches 签名依赖 nav 缓存）
    mount(
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
    mount(
        server,
        "/xlive/app-room/v2/guardTab/topListNew",
        json_ok(json!({
            "top3": [],
            "list": [
                {"uid": 1001, "username": "观众A", "face": "f1", "guard_level": 3, "medal_level": 12, "rank": 1},
                {"uid": 1002, "username": "观众B", "guard_level": 0, "medal_level": 3, "rank": 2},
            ]
        })),
    )
    .await;
    mount(
        server,
        "/x/relation/stat",
        json_ok(json!({"following": 5, "follower": 9})),
    )
    .await;
    mount(
        server,
        "/x/space/wbi/acc/info",
        json_ok(json!({"name": "昵称", "face": "face", "sign": "签名", "level": 5})),
    )
    .await;
    mount(
        server,
        "/x/relation/followings",
        json_ok(json!({"list": [{"mid": 2001, "uname": "关注1", "sign": "s"}]})),
    )
    .await;
    mount(
        server,
        "/x/space/wbi/arc/search",
        json_ok(json!({"list": {"vlist": [
            {"aid": 80433022, "bvid": "BV1xx", "title": "投稿1", "description": "d", "created": 1700000000}
        ]}})),
    )
    .await;
    mount(
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
    mount(
        server,
        "/x/v3/fav/folder/created/list-all",
        json_ok(json!({"list": [{"id": 555, "title": "默认收藏夹", "media_count": 2, "attr": 0}]})),
    )
    .await;
    mount(
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
    // bangumi → 404：单源失败隔离测试点
    mount(
        server,
        "/x/space/bangumi/follow/list",
        ResponseTemplate::new(404),
    )
    .await;
    mount(
        server,
        "/x/space/lastplaygame/v2",
        json_ok(json!({"list": [{"name": "游戏A", "game_id": 7, "summary": "好玩"}]})),
    )
    .await;
    mount(
        server,
        "/x/web-interface/view",
        json_ok(json!({
            "tid": 167, "tname": "知识", "parent_tid": 36, "tname_v2": "科普",
            "title": "T", "pubdate": 1700000000, "owner": {"mid": 3001, "name": "up1"}
        })),
    )
    .await;
    mount(
        server,
        "/x/tag/archive/tags",
        json_ok(json!([{"tag_name": "tagA"}, {"name": "tagB"}])),
    )
    .await;
    mount(
        server,
        "/x/web-interface/wbi/search/square",
        json_ok(json!({"trending": {"list": [{"keyword": "热词1"}]}})),
    )
    .await;
    // M2-B2c 房间级采集点
    mount(
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
    mount(
        server,
        "/xlive/web-room/v1/record/getList",
        json_ok(json!({"count": 2, "list": [
            {"rid": "R1Ex", "title": "回放A", "area_name": "虚拟主播", "parent_area_name": "娱乐",
             "start_timestamp": 1700000000, "end_timestamp": 1700000200, "danmu_num": 2, "length": 120},
            {"rid": "R2Ex", "title": "回放B", "area_name": "虚拟主播", "parent_area_name": "娱乐",
             "start_timestamp": 1700000400, "end_timestamp": 1700000600, "danmu_num": 1, "length": 60}
        ]})),
    )
    .await;
    mount(
        server,
        "/xlive/web-room/v1/record/getInfoByLiveRecord",
        json_ok(
            json!({"live_record_info": {"rid": "R1Ex"}, "dm_info": {"num": 1, "total_num": 2}}),
        ),
    )
    .await;
    mount(
        server,
        "/xlive/web-room/v1/dM/getDMMsgByPlayBackID",
        json_ok(json!({"dm": {"dm_info": [
            {"text": "弹幕一", "uid": 998877, "medal": {"medal_name": "牌子", "medal_level": 7}},
            {"text": "弹幕二", "uid": 998878, "medal": null}
        ]}})),
    )
    .await;
}

fn test_config(root: &Path, budget: i64) -> Config {
    Config {
        source: root.join("config.yaml"),
        project_name: "test-project".into(),
        output_dir: root.to_path_buf(),
        bilibili: BilibiliConfig {
            room_id: "983".into(),
            streamer_uid: "9001".into(),
            cookie: "SESSDATA=test".into(),
            additional_viewer_ids: vec!["1003".into()],
        },
        collection: CollectionConfig {
            max_guards: 2,
            per_viewer_request_budget: budget,
            followings_limit: 2,
            recent_videos: 1,
            recent_dynamics: 1,
            favorite_folders: 1,
            favorite_items_per_folder: 2,
            bangumi_limit: 1,
            games_limit: 1,
            max_video_metadata_items: 5,
            request_delay_seconds: 0.0,
            timeout_seconds: 5.0,
            room_comment_request_budget: 3,
            live_replay_danmaku_limit: 2,
            lead_fetch_budget_per_run: 0,
            leads_autonomy: 0,
        },
        perception: PerceptionConfig {
            max_evidence_per_viewer: 1000,
            preserve_raw_snapshots: true,
            platform_hot_search_limit: 5,
            minimum_community_size: 1,
            peer: PeerDiscoveryConfig {
                candidate_limit: 20,
                recent_videos: 8,
                recent_dynamics: 8,
                max_formal_peers: 8,
            },
        },
        ai: AiConfig {
            api: "chat_completions".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            timeout_seconds: 1.0,
            max_output_tokens: 1024,
            reasoning: ReasoningConfig {
                enabled: false,
                effort: "high".into(),
                replay_content: true,
            },
            agent: AgentRuntimeConfig {
                max_turns: 2,
                resume: false,
                local_trace: false,
                run_retries: 0,
                retry_backoff_seconds: 0.0,
                viewer_token_budget: 200_000,
            },
            search_results_per_query: 20,
            rules: vec![],
        },
        report_title: "t".into(),
    }
}

async fn run_collect(
    server: MockServer,
    root: &Path,
    budget: i64,
    mode: CollectMode,
) -> Result<serde_json::Value, CollectError> {
    let base = server.uri();
    let mut config = test_config(root, budget);
    if let CollectMode::SingleViewer(uid) = &mode {
        config.bilibili.additional_viewer_ids = vec![uid.clone()];
    }
    tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(client, &config, mode, &mut |_msg: &str| {})
    })
    .await
    .expect("task join")
}

fn read(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("file readable"))
        .expect("valid json")
}

#[tokio::test(flavor = "multi_thread")]
async fn collect_full_run_happy_path() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let summary = run_collect(server, root, 12, CollectMode::Guards)
        .await
        .expect("collect ok");

    // collection.json 完成态
    let collection = read(&root.join("collection.json"));
    assert_eq!(collection["status"], "complete");
    // M4.x-T1 冻结：零消费（budget=0 + 空账本）→ leads_consumed 键缺席
    // （schema = {缺席, 正整数 i64}，消费者按显式 i64、缺省 0 解读）。
    assert!(
        collection.get("leads_consumed").is_none(),
        "零消费不得面世 leads_consumed 键：{collection}"
    );
    assert_eq!(
        summary["authenticated_uid"], "42",
        "数字 mid 必须转字符串（批次D 修 S1）"
    );
    assert_eq!(summary["viewer_count"], 3);
    assert_eq!(summary["guard_count"], 2);
    assert_eq!(summary["manual_viewer_count"], 1);
    let counts = summary["source_status_counts"].as_object().unwrap();
    assert_eq!(counts["bangumi:error"], 3, "bangumi 404 全员隔离为 error");
    assert_eq!(counts["profile:ok"], 3);
    assert_eq!(counts["coins:unsupported"], 3);
    assert!(summary["request_count"].as_i64().unwrap() >= 10);

    // 观众文件：3 份、sources 结构、错误隔离
    for uid in ["1001", "1002", "1003"] {
        let viewer = read(&root.join("viewers").join(format!("{uid}.json")));
        assert_eq!(viewer["schema_version"], 1);
        assert_eq!(
            viewer["viewer"]["name"], "昵称",
            "profile 名称盖过空 seed 名"
        );
        assert_eq!(viewer["sources"]["followings"]["status"], "ok");
        assert_eq!(viewer["sources"]["followings"]["count"], 1);
        assert_eq!(viewer["sources"]["bangumi"]["status"], "error");
        assert_eq!(viewer["sources"]["coins"]["status"], "unsupported");
        assert_eq!(viewer["sources"]["favorites"]["count"], 1);
        // enrich 回灌：收藏视频带上 B站 TAG + 分区
        let fav = &viewer["sources"]["favorites"]["items"][0];
        assert_eq!(fav["tags"], json!(["tagA", "tagB"]));
        assert_eq!(fav["platform_category"]["name"], "知识");
        let video = &viewer["sources"]["videos"]["items"][0];
        assert_eq!(video["tags"], json!(["tagA", "tagB"]));
        assert_eq!(video["platform_category"]["id"], 167);
    }

    // 视频元数据缓存：收藏(priority 0) 与投稿(priority 2) 都在
    let metadata = read(&root.join("shared").join("video_metadata.json"));
    assert!(metadata.get("BV1xx").is_some() && metadata.get("BV2yy").is_some());

    // 平台快照：分区/TAG/热搜
    let snapshot = read(&root.join("shared").join("platform_snapshot.json"));
    assert_eq!(snapshot["platform"], "bilibili");
    assert_eq!(snapshot["observed_video_tags"], json!(["tagA", "tagB"]));
    assert_eq!(snapshot["observed_video_categories"][0]["name"], "知识");
    assert_eq!(snapshot["hot_searches"][0]["keyword"], "热词1");
    assert_eq!(summary["video_metadata_items"], 2);
    assert_eq!(summary["platform_hot_searches"], 1);

    // 主播文件
    let streamer = read(&root.join("streamer.json"));
    assert_eq!(streamer["profile"]["name"], "昵称");
    assert_eq!(streamer["statuses"]["videos"], "ok");
    assert_eq!(
        streamer["sources"]["videos"][0]["tags"],
        json!(["tagA", "tagB"])
    );

    // 手工观众 seed_source
    let manual = read(&root.join("viewers").join("1003.json"));
    assert_eq!(manual["viewer"]["seed_source"], "manual");

    // 写盘键序 = Python dict 插入序（serde_json preserve_order 的面：字节 parity）
    let raw = std::fs::read_to_string(root.join("viewers").join("1001.json")).unwrap();
    let positions: Vec<usize> = [
        "schema_version",
        "collected_at",
        "viewer",
        "profile",
        "sources",
        "request_budget",
        "source_operations_used",
    ]
    .iter()
    .map(|key| raw.find(key).unwrap_or_else(|| panic!("missing key {key}")))
    .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "viewers/1001.json 键序必须按插入序：{positions:?}"
    );

    // 房间级浅存在：评论区
    let comments = read(&root.join("shared").join("room_comments.json"));
    assert_eq!(comments["status"], "ok");
    assert_eq!(comments["count"], 2, "视频+动态各 1 目标 × 1 行");
    assert_eq!(
        summary["coverage"]["video_comment_requests"], 4,
        "发现2+回复2"
    );

    // 回放列表 + 回放弹幕
    let records = read(&root.join("shared").join("live_records.json"));
    assert_eq!(records["status"], "ok");
    assert_eq!(records["records"][0]["rid"], "R1Ex");
    assert_eq!(summary["coverage"]["live_records"], 2);
    let danmaku = read(&root.join("shared").join("replay_danmaku.json"));
    assert_eq!(danmaku["status"], "ok");
    assert_eq!(danmaku["record_count"], 2);
    assert_eq!(danmaku["line_count"], 4);
    assert_eq!(
        summary["coverage"]["replay_danmaku_requests"], 4,
        "每场 info1+分片1"
    );
    let bundle = &danmaku["records"][0];
    assert_eq!(bundle["messages"][0]["text"], "弹幕一");
    assert_eq!(bundle["messages"][0]["uid"], 998877);

    // 浅存在 uid 边界：评论 8877 / 弹幕 998877 不得进入深采池
    let viewer_files: Vec<_> = std::fs::read_dir(root.join("viewers"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        viewer_files.len(),
        3,
        "viewers/ 只能有种子三人，实际 {viewer_files:?}"
    );
    for file in &viewer_files {
        let body = std::fs::read_to_string(root.join("viewers").join(file)).unwrap();
        assert!(
            !body.contains("998877") && !body.contains("8877"),
            "{file} 泄漏浅存在 uid"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn budget_exhausted_marks_all_later_sources_skipped() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // budget=3：profile + relation_stat + followings 之后全部 budget_skipped
    let summary = run_collect(server, root, 3, CollectMode::Guards)
        .await
        .expect("collect ok");
    let viewer = read(&root.join("viewers").join("1001.json"));
    assert_eq!(viewer["sources"]["profile"]["status"], "ok");
    assert_eq!(viewer["sources"]["relation_stat"]["status"], "ok");
    assert_eq!(viewer["sources"]["followings"]["status"], "ok");
    for name in ["videos", "dynamics", "favorites", "bangumi", "games"] {
        assert_eq!(
            viewer["sources"][name]["status"], "budget_skipped",
            "{name} should be budget_skipped"
        );
    }
    assert_eq!(
        viewer["sources"]["videos"]["detail"],
        "per-viewer request budget exhausted"
    );
    assert_eq!(summary["source_status_counts"]["videos:budget_skipped"], 3);
    // 没有 content bvid → enrich 不发请求、缓存为空
    assert_eq!(summary["video_metadata_items"], 0);
    // nav1 + guards1 + 3观众×3 + enrich0 + 热搜1 + 主播4 + 房间级9(评论发现2+回复2+回放列表1+弹幕4) = 25（预算截断后零额外调用）
    assert_eq!(summary["request_count"], 25);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_login_writes_failed_status() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/x/web-interface/nav",
        json_ok(json!({"isLogin": false, "mid": 0, "uname": ""})),
    )
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let base = server.uri();
    let config = test_config(root, 12);
    let err = tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(client, &config, CollectMode::Guards, &mut |_msg: &str| {})
    })
    .await
    .expect("task join")
    .unwrap_err();
    assert!(matches!(err, CollectError::Message(_)), "{err:?}");
    let collection = read(&root.join("collection.json"));
    assert_eq!(collection["status"], "failed");
    assert!(
        collection["detail"]
            .as_str()
            .unwrap()
            .contains("not logged in")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streamer_only_mode_skips_viewer_pool() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let summary = run_collect(server, root, 12, CollectMode::StreamerOnly)
        .await
        .expect("collect ok");
    assert!(
        !root.join("viewers").exists(),
        "streamer-only 不得产出深采池文件"
    );
    assert_eq!(summary["viewer_count"], 0);
    assert_eq!(summary["guard_count"], 0);
    // 房间级语料照跑（冷启动 T0 供能：主播采集 + 回放弹幕 + 评论区浅存在）
    let streamer = read(&root.join("streamer.json"));
    assert_eq!(streamer["statuses"]["videos"], "ok");
    let danmaku = read(&root.join("shared").join("replay_danmaku.json"));
    assert_eq!(danmaku["line_count"], 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_danmaku_limit_caps_records() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let base = server.uri();
    let mut config = test_config(root, 12);
    config.collection.live_replay_danmaku_limit = 1;
    tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(
            client,
            &config,
            CollectMode::StreamerOnly,
            &mut |_msg: &str| {},
        )
    })
    .await
    .expect("task join")
    .expect("collect ok");
    let danmaku = read(&root.join("shared").join("replay_danmaku.json"));
    assert_eq!(danmaku["record_count"], 1, "limit=1 只拉第一场");
    assert_eq!(danmaku["records"][0]["rid"], "R1Ex");
    assert_eq!(danmaku["line_count"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn single_viewer_mode_collects_one_manual_viewer() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let summary = run_collect(server, root, 12, CollectMode::SingleViewer("1003".into()))
        .await
        .expect("collect ok");
    let files: Vec<_> = std::fs::read_dir(root.join("viewers"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files, ["1003.json"], "单查只产单独观众的深采文件");
    let viewer = read(&root.join("viewers").join("1003.json"));
    assert_eq!(viewer["viewer"]["seed_source"], "manual");
    assert_eq!(summary["viewer_count"], 1);
}

/// 险口 #1 回归钉：单查（SingleViewer）不得清场——archive 与 reset_output
/// 都必须跳过，只覆写目标 uid 的 viewers/<uid>.json；其余舰长事实、site、
/// shared 与 ai/ 哨兵一律字节级原样。
///
/// R2 强化：
/// - u2 预栽升级为「带 sources.videos items（含 bvid）的完整舰长壳」——pre-fix
///   enrich 无差别全量会把 u2 也回写（tags/platform_category 进件 → u2 字节翻）
///   → u2 字节不变断言即是 F2 的红点。
/// - 成功轮再断言 collection.json 口径诚实：viewer_count == 盘面文件数 2
///   （u1+u2 都在盘），不是 seed 人数 1——F1 成功口径的红点。
#[tokio::test(flavor = "multi_thread")]
async fn single_viewer_preserves_other_viewers_site_and_shared() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // 预栽：两个观众事实 + site/shared/ai 哨兵（各写入识别串）
    let u1_seed = "{\"viewer\":{\"id\":\"u1\",\"name\":\"预栽旧事实\"}}";
    let u2_seed_value = json!({
        "viewer": {"id": "u2", "name": "舰长B待保"},
        "sources": {
            "videos": {
                "status": "ok",
                "count": 1,
                "items": [
                    {"source": "video", "bvid": "BV14ff", "title": "舰长B投稿"}
                ]
            }
        }
    });
    let u2_seed = serde_json::to_string(&u2_seed_value).unwrap();
    std::fs::create_dir_all(root.join("viewers")).unwrap();
    std::fs::write(root.join("viewers").join("u1.json"), u1_seed).unwrap();
    std::fs::write(root.join("viewers").join("u2.json"), &u2_seed).unwrap();
    std::fs::create_dir_all(root.join("site")).unwrap();
    std::fs::write(root.join("site").join("sentinel.txt"), "SITE-哨兵99").unwrap();
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::write(root.join("shared").join("sentinel.txt"), "SHARED-哨兵99").unwrap();
    std::fs::create_dir_all(root.join("ai")).unwrap();
    std::fs::write(root.join("ai").join("sentinel.txt"), "AI-哨兵99").unwrap();

    // 单查 u1：用既有 wiremock 布景完整采集一轮（BV14ff 复用 mount_baseline 的
    // video_detail/video_tags 铺法——路径匹配与 bvid 无关，天然点亮 enrich 回写路径）
    let summary = run_collect(server, root, 12, CollectMode::SingleViewer("u1".into()))
        .await
        .expect("collect ok");
    // R2-F1 口径：结果摘要的 viewer_count 也走盘面（=2），不是 seed=1
    assert_eq!(summary["viewer_count"], 2);

    // (a) 其余舰长事实字节级原样
    assert_eq!(
        std::fs::read_to_string(root.join("viewers").join("u2.json")).unwrap(),
        u2_seed,
        "单查不得销毁其余舰长的采集事实"
    );
    // (b) site/shared/ai 哨兵字节级原样
    for (rel, want) in [
        ("site/sentinel.txt", "SITE-哨兵99"),
        ("shared/sentinel.txt", "SHARED-哨兵99"),
        ("ai/sentinel.txt", "AI-哨兵99"),
    ] {
        assert_eq!(
            std::fs::read_to_string(root.join(rel)).unwrap(),
            want,
            "{rel} 哨兵必须字节级原样（单查不清场不归档）"
        );
    }
    // (c) 目标 uid 被重建：存在且为非预栽内容 + 带新采集痕迹
    let raw_u1 = std::fs::read_to_string(root.join("viewers").join("u1.json")).unwrap();
    assert_ne!(raw_u1, u1_seed, "u1.json 必须是本轮重建的结果");
    assert!(!raw_u1.contains("预栽旧事实"), "u1.json 不得残留预栽旧事实");
    let u1 = read(&root.join("viewers").join("u1.json"));
    assert_eq!(u1["viewer"]["id"], "u1");
    assert!(u1.get("collected_at").is_some(), "新采集痕迹：collected_at");
    assert!(u1.get("sources").is_some(), "新采集痕迹：sources 面");

    // (d) R2-F1 成功口径：collection.json 不得落成 seed=1 的空壳——viewer_count
    // 必须等于盘面 viewers/*.json 实际文件数（u1+u2 都在盘 = 2）；guard_count
    // 读不到既有值（本轮冷启动）置 0。
    let collection = read(&root.join("collection.json"));
    assert_eq!(collection["status"], "complete");
    assert_eq!(
        collection["viewer_count"], 2,
        "单查成功轮 collection.json viewer_count 必须=盘面文件数（含未动他人），\
         不得是种子人数 1：{collection}"
    );
    assert_eq!(
        collection["guard_count"], 0,
        "冷启动无既有 collection → guard_count 置 0：{collection}"
    );
}

/// R2-F1 失败径回归钉：单查失败完全不写 collection.json——已经 status==complete
/// 的旧集合字节级保留（失败不杀伤 baseline 门禁）；若冷启动本来就没有则该当保持没有。
#[tokio::test(flavor = "multi_thread")]
async fn single_viewer_failure_preserves_collection_json_gate() {
    let server = MockServer::start().await;
    // 关键端点 nav 500 → 单查在验证登录态即失败（collect_inner 第一步）
    mount(&server, "/x/web-interface/nav", ResponseTemplate::new(500)).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // 预栽一份 status==complete 的上一轮 collection.json（带识别摘要，字节级基线）
    let gate_text = serde_json::to_string_pretty(&json!({
        "status": "complete",
        "project": "test-project",
        "viewer_count": 15,
        "guard_count": 14,
        "detail": "R2-GATE-哨兵",
    }))
    .unwrap();
    std::fs::write(root.join("collection.json"), &gate_text).unwrap();

    let err = run_collect(server, root, 12, CollectMode::SingleViewer("u1".into()))
        .await
        .expect_err("nav 500 必须让单查失败");
    assert!(
        matches!(err, CollectError::Client(_) | CollectError::Message(_)),
        "{err:?}"
    );

    // (a) collection.json 字节级未动（失败不得覆写为 failed / running）
    assert_eq!(
        std::fs::read_to_string(root.join("collection.json")).unwrap(),
        gate_text,
        "单查失败不得改写 collection.json（字节保真）"
    );
    // (b) 门禁可读：read 出来的 status 仍是 complete（baseline 门禁只看该键）
    assert_eq!(
        read(&root.join("collection.json"))["status"],
        "complete",
        "失败后 baseline 门禁仍可读 complete"
    );
}

/// R2-F 的尾段消费闸回归钉：单查模式不得消费已批准线索——无论
/// lead_fetch_budget_per_run 多大，pre-planted approved 行字节级原样，
/// 搜索消费端点零请求，summary 不面世 leads_consumed。
#[tokio::test(flavor = "multi_thread")]
async fn single_viewer_does_not_consume_leads() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    // 消费端点蓄意备好：若 post-fix 仍误闯，必定留下请求痕迹与账本改写
    Mock::given(method("GET"))
        .and(path("/x/web-interface/wbi/search/type"))
        .respond_with(json_ok(json!({"result": [
            {"bvid": "BV1s1", "title": "实机1"}
        ]})))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // 预置账本：1 条 approved（单查不应碰）
    let row = live_core::leads::LedgerRow {
        dedupe_key: "key-single-no-consum".into(),
        lead_type: "search".into(),
        locator: "异环 实机".into(),
        motivation: "m".into(),
        expected_signal: "s".into(),
        priority: "high".into(),
        evidence_ids: vec![],
        viewer_id: "u".into(),
        first_seen_run_id: "run:a".into(),
        created_at: "t".into(),
        status: live_core::leads::LeadStatus::Approved,
        yield_count: 0,
        resolution_note: String::new(),
    };
    let ledger_text = format!("{}\n", serde_json::to_string(&row).unwrap());
    std::fs::write(root.join("leads.jsonl"), &ledger_text).unwrap();

    let base = server.uri();
    let mut config = test_config(root, 12);
    config.collection.lead_fetch_budget_per_run = 1;
    let summary = tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(
            client,
            &config,
            CollectMode::SingleViewer("1003".into()),
            &mut |_msg: &str| {},
        )
    })
    .await
    .expect("task join")
    .expect("collect ok");

    // 账本字节级原样：approved 行未被消费
    assert_eq!(
        std::fs::read_to_string(root.join("leads.jsonl")).unwrap(),
        ledger_text,
        "单查不得消费已批准线索（预算>0 也要停火）"
    );
    // summary/collection 均不面世 leads_consumed
    assert!(
        summary.get("leads_consumed").is_none(),
        "单查成功不得面世 leads_consumed"
    );
    // 消费端点零请求
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| !r.url.path().contains("/search/type")),
        "单查不得触发搜索消费请求"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn single_viewer_empty_uid_is_rejected_after_login() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/x/web-interface/nav",
        json_ok(json!({"isLogin": true, "mid": 42, "uname": "me"})),
    )
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let base = server.uri();
    let config = test_config(root, 12);
    let err = tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(
            client,
            &config,
            CollectMode::SingleViewer("  ".into()),
            &mut |_msg: &str| (),
        )
    })
    .await
    .expect("task join")
    .expect_err("empty uid must be rejected");
    assert!(matches!(err, CollectError::Message(_)), "{err:?}");
    let requests = server.received_requests().await.unwrap();
    let only_nav = requests
        .iter()
        .all(|r| r.url.path() == "/x/web-interface/nav");
    assert!(
        only_nav,
        "空 uid 必须在 guard 抽取前失败，requests={requests:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn room_comment_budget_zero_disables_points_without_requests() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let base = server.uri();
    let mut config = test_config(root, 12);
    config.collection.room_comment_request_budget = 0;
    let summary = tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(
            client,
            &config,
            CollectMode::StreamerOnly,
            &mut |_msg: &str| (),
        )
    })
    .await
    .expect("task join")
    .expect("collect ok");
    assert_eq!(
        summary["coverage"]["video_comment_requests"], 0,
        "budget=0 不得发任何评论请求"
    );
    let comments = read(&root.join("shared").join("room_comments.json"));
    assert_eq!(comments["status"], "disabled");
    assert_eq!(comments["targets"], serde_json::json!([]));
    // 回放/弹幕不受影响（对称独立开关）
    assert!(summary["coverage"]["live_records"].as_i64().unwrap() >= 0);
}

// ---------------------------------------------------------------------------
// MXA-10（r3-F3 / r5-F2 / r7-环-1）：M4.x 消费环集成钉——approved 账本 +
// budget=1 + wiremock 假搜索面 → 尾段消费写回 + leads_consumed 键 +
// request_count 不漏报。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn collect_tail_consumes_approved_leads_and_recounts_requests() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    // 消费端点：search 型 lead → 3 条结果；只准被请求恰好 1 次
    Mock::given(method("GET"))
        .and(path("/x/web-interface/wbi/search/type"))
        .respond_with(json_ok(json!({"result": [
            {"bvid": "BV1s1", "title": "实机1"},
            {"bvid": "BV1s2", "title": "实机2"},
            {"bvid": "BV1s3", "title": "实机3"}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // 预置账本：1 条 approved（本轮应被消费）+ 1 条 pending（审批闸不得越权）
    let mk_row = |lead_type: &str, locator: &str, status: live_core::leads::LeadStatus| {
        live_core::leads::LedgerRow {
            dedupe_key: format!("key-{locator}"),
            lead_type: lead_type.into(),
            locator: locator.into(),
            motivation: "m".into(),
            expected_signal: "s".into(),
            priority: "high".into(),
            evidence_ids: vec![],
            viewer_id: "u".into(),
            first_seen_run_id: "run:a".into(),
            created_at: "t".into(),
            status,
            yield_count: 0,
            resolution_note: String::new(),
        }
    };
    let ledger_text = format!(
        "{}\n{}\n",
        serde_json::to_string(&mk_row(
            "search",
            "异环 实机",
            live_core::leads::LeadStatus::Approved
        ))
        .unwrap(),
        serde_json::to_string(&mk_row(
            "video",
            "BVpending",
            live_core::leads::LeadStatus::PendingApproval
        ))
        .unwrap()
    );
    std::fs::write(root.join("leads.jsonl"), ledger_text).unwrap();

    let base = server.uri();
    let mut config = test_config(root, 12);
    config.collection.lead_fetch_budget_per_run = 1;
    let summary = tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(client, &config, CollectMode::Guards, &mut |_msg: &str| {})
    })
    .await
    .expect("task join")
    .expect("collect ok");

    // 消费键：预算 1 → 恰消费 1 条
    assert_eq!(summary["leads_consumed"], 1);
    // MXA-2：request_count = 本进程向 mock 服务器发出的全部请求（含消费那一次）
    let total_requests = server.received_requests().await.expect("requests").len() as i64;
    assert_eq!(
        summary["request_count"].as_i64().unwrap(),
        total_requests,
        "消费请求必须计入 request_count"
    );

    // 账本写回：approved → consumed（yield=3）；pending 行原样不动
    let rows = live_core::leads::read_ledger(&root.join("leads.jsonl"));
    assert_eq!(rows.len(), 2, "账本行数不变");
    let consumed_row = rows
        .iter()
        .find(|r| r.locator == "异环 实机")
        .expect("approved 行仍在");
    assert_eq!(consumed_row.status, live_core::leads::LeadStatus::Consumed);
    assert_eq!(consumed_row.yield_count, 3, "yield = 搜索结果条数");
    assert!(consumed_row.resolution_note.is_empty());
    let pending_row = rows
        .iter()
        .find(|r| r.locator == "BVpending")
        .expect("pending 行仍在");
    assert_eq!(
        pending_row.status,
        live_core::leads::LeadStatus::PendingApproval,
        "审批闸：未批准行不得被消费"
    );

    // collection.json 落盘一致性
    let collection = read(&root.join("collection.json"));
    assert_eq!(collection["leads_consumed"], 1);
    assert_eq!(
        collection["request_count"].as_i64().unwrap(),
        total_requests
    );
}

// ---------------------------------------------------------------------------
// G2-B（工作项 3）：L1 自治（collection.leads_autonomy）集成钉——
// 自动批准（谓词 = creator/search 且 creator 目标 uid 不在本房间名册）
// → 照常预算消费 → 账本记「L1 自动」痕。L0 = 现状纯人工，一字不动。
// ---------------------------------------------------------------------------

fn leads_row(
    lead_type: &str,
    locator: &str,
    status: live_core::leads::LeadStatus,
) -> live_core::leads::LedgerRow {
    live_core::leads::LedgerRow {
        dedupe_key: format!("key-{locator}"),
        lead_type: lead_type.into(),
        locator: locator.into(),
        motivation: "m".into(),
        expected_signal: "s".into(),
        priority: "high".into(),
        evidence_ids: vec![],
        viewer_id: "audience".into(),
        first_seen_run_id: "run:a".into(),
        created_at: "t".into(),
        status,
        yield_count: 0,
        resolution_note: String::new(),
    }
}

/// L1 链：pending creator（3001，不在名册 {9001,1001,1002,1003}）+ autonomy=1
/// + 预算>0 → 尾段先自动批准（落账本记 L1 痕）再照常消费 → consumed + yield；
/// 同账本的 pending video 谓词拒位（类型不符）原样保留。
#[tokio::test(flavor = "multi_thread")]
async fn l1_autonomy_auto_approves_and_consumes_new_creator() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(
        root.join("leads.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::to_string(&leads_row(
                "creator",
                "3001",
                live_core::leads::LeadStatus::PendingApproval
            ))
            .unwrap(),
            serde_json::to_string(&leads_row(
                "video",
                "BVpending",
                live_core::leads::LeadStatus::PendingApproval
            ))
            .unwrap()
        ),
    )
    .unwrap();

    let base = server.uri();
    let mut config = test_config(root, 12);
    config.collection.lead_fetch_budget_per_run = 1;
    config.collection.leads_autonomy = 1;
    let rings = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = rings.clone();
    let summary = tokio::task::spawn_blocking(move || {
        let mut emit = move |msg: &str| sink.lock().unwrap().push(msg.to_string());
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(client, &config, CollectMode::Guards, &mut emit)
    })
    .await
    .expect("task join")
    .expect("collect ok");

    // 链闭合：自动批准 → 预算消费 → consumed + yield 落袋
    assert_eq!(summary["leads_consumed"], 1, "{summary}");
    let rows = live_core::leads::read_ledger(&root.join("leads.jsonl"));
    assert_eq!(rows.len(), 2, "账本行数不变");
    let creator = rows
        .iter()
        .find(|r| r.lead_type == "creator")
        .expect("creator 行");
    assert_eq!(
        creator.status,
        live_core::leads::LeadStatus::Consumed,
        "{creator:?}"
    );
    assert!(creator.yield_count > 0, "消费带回 yield：{creator:?}");
    assert!(
        creator.resolution_note.is_empty(),
        "消费成功清痕（L1 痕已兑现为消费）：{creator:?}"
    );
    // 谓词拒位：video 型 pending 不被 L1 自动批准（类型不符，永远人工域）
    let video = rows
        .iter()
        .find(|r| r.lead_type == "video")
        .expect("video 行");
    assert_eq!(
        video.status,
        live_core::leads::LeadStatus::PendingApproval,
        "video 型 pending 不得被 L1 批：{video:?}"
    );
    // L1 自动批准有响铃留痕（克隆后即刻放锁——不得持锁跨 await，clippy::await_holding_lock）
    let rings = rings.lock().unwrap().clone();
    assert!(
        rings.iter().any(|m| m.contains("L1")),
        "L1 自动批准必须响铃：{rings:?}"
    );
    // 消费的真实网络面：creator 3001 命中 arc/search（mid=3001）
    let requests = server.received_requests().await.expect("requests");
    assert!(
        requests.iter().any(|r| {
            r.url.path() == "/x/space/wbi/arc/search"
                && r.url.query_pairs().any(|(k, v)| k == "mid" && v == "3001")
        }),
        "creator 消费必须携带 mid=3001"
    );
}

/// L0 一字不动：默认 autonomy=0（不显式设置）即使预算>0，pending 行也原样
/// 保留、零消费请求、summary 不面世 leads_consumed、账本字节级不动。
#[tokio::test(flavor = "multi_thread")]
async fn l0_autonomy_default_pending_never_auto_approved() {
    let server = MockServer::start().await;
    mount_baseline(&server).await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let row = leads_row(
        "creator",
        "3001",
        live_core::leads::LeadStatus::PendingApproval,
    );
    let ledger_text = format!("{}\n", serde_json::to_string(&row).unwrap());
    std::fs::write(root.join("leads.jsonl"), &ledger_text).unwrap();

    let base = server.uri();
    let mut config = test_config(root, 12);
    config.collection.lead_fetch_budget_per_run = 1;
    let summary = tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::with_origin(&base, &base, "SESSDATA=test", 0.0, 5.0).unwrap();
        collect_with_client(client, &config, CollectMode::Guards, &mut |_msg: &str| {})
    })
    .await
    .expect("task join")
    .expect("collect ok");

    assert_eq!(
        std::fs::read_to_string(root.join("leads.jsonl")).unwrap(),
        ledger_text,
        "L0 账本字节级不动（无消费也无自动批准）"
    );
    assert!(
        summary.get("leads_consumed").is_none(),
        "L0 零消费不得面世 leads_consumed：{summary}"
    );
    let requests = server.received_requests().await.expect("requests");
    assert!(
        !requests.iter().any(|r| {
            r.url.path() == "/x/space/wbi/arc/search"
                && r.url.query_pairs().any(|(k, v)| k == "mid" && v == "3001")
        }),
        "L0 不得发 creator 消费请求"
    );
}
