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
