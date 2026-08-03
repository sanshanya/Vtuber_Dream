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
