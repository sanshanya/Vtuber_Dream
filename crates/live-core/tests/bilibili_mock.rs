//! bilibili.rs 的 wiremock 负例 + 分页硬语义测试（design M2 验收点）：
//! HTTP 412 / API code -352 / HTTP 429 / v_voucher 挑战、
//! followings/guard_members 多页拼接不截断 + 成员去重。
//! 客户端延迟=0；根地址指向 MockServer（生产路径与真地址相同，仅根不同）。

use live_core::bilibili::{BilibiliClient, BilibiliError, DANMAKU_SHARD_CAP};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &str) -> Result<BilibiliClient, BilibiliError> {
    BilibiliClient::with_origin(server, server, "SESSDATA=test", 0.0, 5.0)
}

async fn mount_guard_stub(server: &MockServer, template: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/xlive/app-room/v2/guardTab/topListNew"))
        .respond_with(template)
        .mount(server)
        .await;
}

async fn call<F, T>(server: MockServer, f: F) -> Result<T, BilibiliError>
where
    F: FnOnce(&mut BilibiliClient) -> Result<T, BilibiliError> + Send + 'static,
    T: Send + 'static,
{
    let address = server.uri();
    tokio::task::spawn_blocking(move || f(&mut client(&address).unwrap()))
        .await
        .expect("task join")
}

#[tokio::test(flavor = "multi_thread")]
async fn http_412_is_http_error_with_endpoint() {
    let server = MockServer::start().await;
    mount_guard_stub(
        &server,
        ResponseTemplate::new(412).set_body_json(json!({"message": "risk"})),
    )
    .await;
    let result = call(server, |client| client.guard_members("983", "128", 10)).await;
    assert!(
        matches!(result, Err(BilibiliError::Http { status: 412, ref endpoint }) if endpoint.contains("guardTab")),
        "412 必须归类为 Http 错误: {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn api_minus_352_is_api_error_not_hidden() {
    let server = MockServer::start().await;
    mount_guard_stub(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"code": -352, "message": "risk -352"})),
    )
    .await;
    let result = call(server, |client| client.guard_members("983", "128", 10)).await;
    match result {
        Err(BilibiliError::Api { code, message, .. }) => {
            assert_eq!(code, -352);
            assert_eq!(message, "risk -352");
        }
        other => panic!("-352 必须归类为 Api 错误: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_429_is_http_error() {
    let server = MockServer::start().await;
    mount_guard_stub(&server, ResponseTemplate::new(429)).await;
    let result = call(server, |client| client.guard_members("983", "128", 10)).await;
    assert!(
        matches!(result, Err(BilibiliError::Http { status: 429, .. })),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn data_v_voucher_only_is_voucher_challenge() {
    let server = MockServer::start().await;
    mount_guard_stub(
        &server,
        ResponseTemplate::new(200)
            .set_body_json(json!({"code": 0, "data": {"v_voucher": "v.2.4.8"}})),
    )
    .await;
    let result = call(server, |client| client.guard_members("983", "128", 10)).await;
    assert!(
        matches!(result, Err(BilibiliError::Voucher { .. })),
        "{result:?}"
    );
}

// ---------------------------------------------------------------------------
// 分页不截断：关注列表 50+50+30 三页，limit 溢出时只被 limit 截断
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn followings_paginate_to_limit_without_possible_truncation() {
    let server = MockServer::start().await;
    for page in 1..=3 {
        let size = if page <= 2 { 50 } else { 30 };
        let list: Vec<_> = (0..size)
            .map(|index| json!({"mid": page * 1000 + index, "uname": format!("u-{page}-{index}")}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/x/relation/followings"))
            .and(query_param("pn", page.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"code": 0, "data": {"list": list}})),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let rows = call(server, |client| client.followings("321", 130))
        .await
        .unwrap();
    assert_eq!(rows.len(), 130, "分页必须拼满 limit 再截断");
    assert_eq!(rows[129]["mid"].as_i64().unwrap(), 3000 + 29);
}

// ---------------------------------------------------------------------------
// 大航海：top3 + 分页去重 + limit 提前停页
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn guard_members_merge_top3_first_page_and_dedupe() {
    let server = MockServer::start().await;
    let mut top3 = Vec::new();
    for index in 0..3 {
        top3.push(json!({
            "uid": 1000 + index,
            "uinfo": {"base": {"name": format!("top-{index}"), "face": "https://i2.hdslb.com/f.png"},
                       "medal": {"guard_level": 3, "level": 10}, "guard": {"level": 3}},
        }));
    }
    let mut list_page1 = vec![top3[0].clone()]; // 与 top3 重复 → 去重
    for index in 1..20 {
        list_page1.push(json!({
            "uid": 2000 + index,
            "uinfo": {"base": {"name": format!("m-{index}")}, "medal": {"level": 3}, "guard": {"level": 2}},
        }));
    }
    Mock::given(method("GET"))
        .and(path("/xlive/app-room/v2/guardTab/topListNew"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "data": {"top3": top3, "list": list_page1},
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/app-room/v2/guardTab/topListNew"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "data": {"list": []},
        })))
        .mount(&server)
        .await;
    let members = call(server, |client| client.guard_members("2", "100655", 10))
        .await
        .unwrap();
    let uids: Vec<_> = members
        .iter()
        .map(|row| row["uid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(uids.len(), 10, "limit 命中即停");
    let unique: std::collections::BTreeSet<_> = uids.iter().cloned().collect();
    assert_eq!(unique.len(), uids.len(), "top3 名单必须去重: {uids:?}");
    assert!(uids.contains(&"1000".to_string()), "top3 第一位应保留");
}

// ---------------------------------------------------------------------------
// 404 是 Http 错误（非 Api）：与 412/429 同族
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn http_404_is_http_error() {
    let server = MockServer::start().await;
    let result = call(server, |client| client.video_detail("BV1u14LzIEYm")).await;
    assert!(
        matches!(result, Err(BilibiliError::Http { status: 404, .. })),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_records_two_pages_then_cap() {
    let server = MockServer::start().await;
    // 页 1 满 20 → 翻页；页 2 不满即停（共 25 条，2 请求 —— 上限 MAX_PAGES=2）
    let page = |n: i64| {
        let items: Vec<serde_json::Value> =
            (0..n).map(|i| json!({"rid": format!("R{i}")})).collect();
        ResponseTemplate::new(200)
            .set_body_json(json!({"code": 0, "data": {"count": 99, "list": items}}))
    };
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getList"))
        .and(query_param("page", "1"))
        .respond_with(page(20))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getList"))
        .and(query_param("page", "2"))
        .respond_with(page(5))
        .expect(1)
        .mount(&server)
        .await;
    let rows = call(server, |client| client.live_records("983", 20))
        .await
        .expect("records");
    assert_eq!(rows.len(), 25);
    assert_eq!(rows[24]["rid"], "R4");
}

#[tokio::test(flavor = "multi_thread")]
async fn live_records_caps_at_two_pages() {
    let server = MockServer::start().await;
    let page = |n: i64| {
        let items: Vec<serde_json::Value> =
            (0..n).map(|i| json!({"rid": format!("R{i}")})).collect();
        ResponseTemplate::new(200)
            .set_body_json(json!({"code": 0, "data": {"count": 999, "list": items}}))
    };
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getList"))
        .and(query_param("page", "1"))
        .respond_with(page(20))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getList"))
        .and(query_param("page", "2"))
        .respond_with(page(20))
        .mount(&server)
        .await;
    let uri = server.uri();
    let result = tokio::task::spawn_blocking(move || client(&uri).unwrap().live_records("983", 20))
        .await
        .expect("task join");
    let requests = server.received_requests().await.unwrap();
    let pages: Vec<String> = requests
        .iter()
        .map(|r| r.url.query().unwrap_or("<none>").to_string())
        .collect();
    let rows = result.expect("records");
    assert_eq!(rows.len(), 40, "MAX_PAGES=2 封顶");
    assert_eq!(pages.len(), 2, "恰好 2 次请求，不追第 3 页 pages={pages:?}");
}

/// DANMAKU_SHARD_CAP：异常 num=1_000_000_000 必须被钳到 200，切片请求数钉死（安全批 R1）。
#[tokio::test(flavor = "multi_thread")]
async fn live_record_danmaku_shard_cap_clamps_amplification() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getInfoByLiveRecord"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"dm_info": {"num": 1_000_000_000, "total_num": 1}}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/dM/getDMMsgByPlayBackID"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"dm": {"dm_info": [{"text": "x"}]}}})),
        )
        .expect(DANMAKU_SHARD_CAP as u64)
        .mount(&server)
        .await;
    let messages = call(server, |client| client.live_record_danmaku("R1cap"))
        .await
        .expect("danmaku");
    assert_eq!(messages.len(), DANMAKU_SHARD_CAP as usize);
}

#[tokio::test(flavor = "multi_thread")]
async fn live_record_danmaku_shards_with_index_and_202_error() {
    // 正常路径：num=2 → 两片按 index 拉取并按序拼接
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getInfoByLiveRecord"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"dm_info": {"num": 2, "total_num": 3}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/dM/getDMMsgByPlayBackID"))
        .and(query_param("index", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"dm": {"dm_info": [{"text": "一"}, {"text": "二"}]}}}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/dM/getDMMsgByPlayBackID"))
        .and(query_param("index", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"dm": {"dm_info": [{"text": "三"}]}}})),
        )
        .mount(&server)
        .await;
    let messages = call(server, |client| client.live_record_danmaku("R1Ex"))
        .await
        .expect("danmaku");
    let texts: Vec<&str> = messages
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["一", "二", "三"], "两片弹幕按分片顺序拼接");

    // 定形负例：旧 rid 已清理 → code=202 需上抛为非 hidden Api 错误（updated 行为）
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getInfoByLiveRecord"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"code": 202, "message": "no such live record", "data": null}),
            ),
        )
        .mount(&server)
        .await;
    let err = call(server, |client| client.live_record_danmaku("OLD_RID"))
        .await
        .expect_err("202 must surface");
    assert!(matches!(err, BilibiliError::Api { code: 202, .. }) && !err.hidden());
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_status_numeric_mid_and_int_login() {
    // Python：str(data.get("mid") or "")——mid 恒为数字也能取到；isLogin=1 → True
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"isLogin": 1, "mid": 349312345, "uname": "我"}}),
        ))
        .mount(&server)
        .await;
    let status = call(server, |client| client.auth_status())
        .await
        .expect("auth status");
    assert_eq!(status["is_login"], true);
    assert_eq!(status["mid"], "349312345"); // 批次D 修复前：数字 mid 恒被判空 ""
    assert_eq!(status["uname"], "我");
}

#[tokio::test(flavor = "multi_thread")]
async fn followings_filters_non_dict_entries_before_page_judgment() {
    // Python：先 isinstance 过滤；页 20 条中 2 条垃圾 → 过滤后 18 < 20 → 不翻页。
    // 若误把垃圾计入页长 → 继续请求 page=2 → 无 mock → 404 出错（本测试的隐形断言）。
    let server = MockServer::start().await;
    let mut items: Vec<serde_json::Value> = (0..18)
        .map(|i| json!({"mid": 100 + i, "uname": format!("u{i}")}))
        .collect();
    items.push(serde_json::Value::Null);
    items.push(json!("junk"));
    Mock::given(method("GET"))
        .and(path("/x/relation/followings"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code": 0, "data": {"list": items}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let rows = call(server, |client| client.followings("128", 20))
        .await
        .expect("followings");
    assert_eq!(rows.len(), 18, "非 dict 条目不得入列");
    assert!(rows.iter().all(|row| row.is_object()));
}

#[tokio::test(flavor = "multi_thread")]
async fn vvoucher_null_is_not_risk_challenge() {
    // Python 要求 data.get("v_voucher") 为真值——null 应变种放行并取 data（此处为 null→Null）
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"v_voucher": null}})),
        )
        .mount(&server)
        .await;
    let data = call(server, |client| client.nav()).await.expect("nav");
    assert_eq!(
        data,
        json!({"v_voucher": null}),
        "v_voucher=null 不得误判风控"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_data_field_returns_null_instead_of_body() {
    // Python：data 键缺席 → None（各端点归一为 {}）——绝不让 code/message 混进结果
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/relation/stat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code": 0, "message": "0"})))
        .mount(&server)
        .await;
    let data = call(server, |client| client.relation_stat("128"))
        .await
        .expect("stat");
    assert!(data.is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn api_message_empty_string_falls_back_to_msg() {
    // Python：message or msg or "unknown error"——空串 message 也回落
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/relation/stat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": -1, "message": "", "msg": "参数错误"})),
        )
        .mount(&server)
        .await;
    let err = call(server, |client| client.relation_stat("128"))
        .await
        .expect_err("api error");
    assert!(
        matches!(&err, BilibiliError::Api { message, .. } if message == "参数错误"),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn favorite_folders_attr_numeric_string_privacy_filter() {
    // Python：int(attr or 0) & 1——数字字符串 "21" 一样判私有被剔除
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/v3/fav/folder/created/list-all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"list": [
                {"id": 1, "attr": "21"}, {"id": 2, "attr": 0}, {"id": 3, "attr": true}
            ]}}),
        ))
        .mount(&server)
        .await;
    let rows = call(server, |client| client.favorite_folders("128", 3))
        .await
        .expect("folders");
    let ids: Vec<i64> = rows.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, [2], "attr=21/true(私有) 剔除；attr=0 保留");
}

#[tokio::test(flavor = "multi_thread")]
async fn video_tags_name_chain_keeps_fallback() {
    // or_fallback 唯一现存调用点的语义钉（tag_name 空时回落 name）
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/tag/archive/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": [{"tag_name": ""}, {"name": "n2"}, {"tag_name": "t1"}]}),
        ))
        .mount(&server)
        .await;
    let tags = call(server, |client| client.video_tags("BV1xx"))
        .await
        .expect("tags");
    // Python：[dict.fromkeys(tags)]——tag_name "" 不入列表
    assert_eq!(tags, vec!["n2".to_string(), "t1".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn videos_paginates_until_limit_or_short_page() {
    // design 修复项"视频列表分页不再截断"：30 + 10 → 40 条不截断
    let server = MockServer::start().await;
    let page = |n: i64, offset: i64| {
        let vlist: Vec<serde_json::Value> = (0..n)
            .map(|i| json!({"bvid": format!("BV{}", offset + i), "title": "t"}))
            .collect();
        ResponseTemplate::new(200)
            .set_body_json(json!({"code": 0, "data": {"list": {"vlist": vlist}}}))
    };
    Mock::given(method("GET"))
        .and(path("/x/space/wbi/arc/search"))
        .and(query_param("pn", "1"))
        .respond_with(page(30, 0))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/space/wbi/arc/search"))
        .and(query_param("pn", "2"))
        .respond_with(page(10, 30))
        .expect(1)
        .mount(&server)
        .await;
    // nav（wbi 签名需要 mixin）
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "wbi_img": {
                    "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                    "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
                }
            }
        })))
        .mount(&server)
        .await;
    let rows = call(server, |client| client.videos("128", 40))
        .await
        .expect("videos");
    assert_eq!(rows.len(), 40, "videos 翻页到 limit 不截断");
    assert_eq!(rows[39]["bvid"], "BV39");
}

// ---------------------------------------------------------------------------
// search_videos 复出负例（E 批次删除时约定：复出与消费者同生并带 wiremock 负例；
// M3-B ResearchService 是该端点的首个消费者）
// ---------------------------------------------------------------------------

fn nav_stub() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"code": 0, "data": {"wbi_img": {
        "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
        "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
    }}}))
}

#[tokio::test(flavor = "multi_thread")]
async fn search_videos_empty_keyword_makes_no_request() {
    let server = MockServer::start().await;
    // 不挂任何 mock：任何请求都会 404 → 早退证明零请求（Python: not keyword.strip() → []）。
    let rows = call(server, |client| {
        client.search_videos("   ", 10, "totalrank")
    })
    .await
    .expect("empty keyword must short-circuit");
    assert!(rows.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn search_videos_api_error_surfaces_as_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(nav_stub())
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/wbi/search/type"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": -412, "message": "请求被拦截"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = call(server, |client| {
        client.search_videos("异环", 5, "totalrank")
    })
    .await;
    assert!(
        matches!(result, Err(BilibiliError::Api { code: -412, .. })),
        "风控 code 必须归类 Api 错误: {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn search_videos_filters_junk_and_truncates_to_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(nav_stub())
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/wbi/search/type"))
        .and(query_param("page_size", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {"result": [
                {"bvid": "BV1", "title": "a"},
                "junk",
                {"bvid": "BV2", "title": "b"},
                {"bvid": "BV3", "title": "c"}
            ]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let rows = call(server, |client| client.search_videos("异环", 2, "pubdate"))
        .await
        .expect("search");
    // 先滤非 dict（junk 不参与条数判定）再截断到 limit=2。
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["bvid"], "BV1");
    assert_eq!(rows[1]["bvid"], "BV2");
}

/// P0-1（迭代细则 v1 §1 地基）：弹幕 shard 必须随行打标——
/// Episode 身份公式 (rid, shard_index, 行序) 依赖上游保序与分片索引；
/// 不打标则无法构造幂等身份。零新增请求：同一拉片循环内打标。
#[tokio::test(flavor = "multi_thread")]
async fn live_record_danmaku_rows_tag_shard_index() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getInfoByLiveRecord"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"dm_info": {"num": 2, "total_num": 3}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/dM/getDMMsgByPlayBackID"))
        .and(query_param("index", "0"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"dm": {"dm_info": [
                {"text": "A1", "uid": "u1"}, {"text": "A2", "uid": "u2"}]}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/dM/getDMMsgByPlayBackID"))
        .and(query_param("index", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"dm": {"dm_info": [{"text": "B1", "uid": "u3"}]}}}),
        ))
        .mount(&server)
        .await;
    let messages = call(server, |client| client.live_record_danmaku("R-shard"))
        .await
        .expect("danmaku");
    assert_eq!(messages.len(), 3);
    let tags: Vec<i64> = messages
        .iter()
        .map(|row| row.get("shard_index").and_then(Value::as_i64).unwrap_or(-1))
        .collect();
    assert_eq!(tags, [0, 0, 1], "每片行必须打贴本片片号: {messages:?}");
}

/// 轮2-R1-A② 红钉：满页全垃圾（uid 全空）时必须按「本轮零新增」收杆——
/// 修前判定只看未过滤 listing.len()：满页恒不满足「页不满」→ page 无限自增死循环。
#[tokio::test(flavor = "multi_thread")]
async fn guard_members_full_pages_of_junk_terminate_on_zero_growth() {
    let server = MockServer::start().await;
    // 满页但全 uid=0（normalize 落 none）：修前「页不满」永不成立 → page 无限
    // 自增（15 秒超时兜底判死循环）。修后：本轮零新增即收杆 → 恰好 1 请求。
    Mock::given(method("GET"))
        .and(path("/xlive/app-room/v2/guardTab/topListNew"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "list": (0..20).map(|_| json!({
                    "uid": 0, "name": "脏行", "medal": null, "guard_level": 1
                })).collect::<Vec<_>>(),
                "top3": []
            },
        })))
        .mount(&server)
        .await;
    let address = server.uri();
    let work = tokio::task::spawn_blocking(move || {
        client(&address).unwrap().guard_members("983", "128", 40)
    });
    let members = tokio::time::timeout(std::time::Duration::from_secs(15), work)
        .await
        .expect("guard_members 15 秒内必须有界返回（修前死循环在这里超时）")
        .expect("task join")
        .expect("guard_members");
    assert!(members.is_empty(), "全垃圾 → 零成员: {members:?}");
    let received = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        received.len(),
        1,
        "零新增即收杆：恰好 1 请求: {} requests",
        received.len()
    );
}
