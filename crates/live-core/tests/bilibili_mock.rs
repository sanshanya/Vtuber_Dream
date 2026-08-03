//! bilibili.rs 的 wiremock 负例 + 分页硬语义测试（design M2 验收点）：
//! HTTP 412 / API code -352 / HTTP 429 / v_voucher 挑战、
//! followings/guard_members 多页拼接不截断 + 成员去重。
//! 客户端延迟=0；根地址指向 MockServer（生产路径与真地址相同，仅根不同）。

use live_core::bilibili::{BilibiliClient, BilibiliError};
use serde_json::json;
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
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xlive/web-room/v1/record/getList"))
        .and(query_param("page", "2"))
        .respond_with(page(5))
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

#[tokio::test(flavor = "multi_thread")]
async fn live_record_danmaku_shards_with_index_and_202_error() {
    // 正常路径：num=2 → 两片，逐片带 index 参数，行内回写 shard_index
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
    assert_eq!(texts, ["一", "二", "三"]);
    assert_eq!(messages[0]["shard_index"], 0);
    assert_eq!(messages[2]["shard_index"], 1);

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
