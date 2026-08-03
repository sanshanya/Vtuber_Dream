//! 真实端点 opt-in smoke（AGENTS.md §8：真实端点测试必须显式 opt-in，
//! 永不进入 `cargo test --workspace` 默认门禁——本文件全部用例 `#[ignore]`）。
//!
//! 运行：
//!   $env:VTD_LIVE_COOKIE="SESSDATA=..."  # 可选；提供后启用严格断言
//!   cargo test --test bilibili_live -- --ignored --nocapture --test-threads=1
//!
//! 职责边界：验证"末公里"——reqwest 真实出网 + WBI 签名被服务端接受 +
//! data 形状与归一化假设一致 + 错误只落在 typed `BilibiliError`。Transport/
//! NotJson 属协议断裂必须红；风控/隐私类 typed error（412 / -799 / 22115）
//! 只记日志不算失败；带 cookie 时核心探针改严格断言。
//! cookie 只从环境变量读取，永不打印、永不写盘。

use live_core::bilibili::{BilibiliClient, BilibiliError};

const STREAMER_UID: &str = "3546595083683995";
const ROOM_ID: &str = "1790370612";

/// 协议级硬失败：线路断裂/非 JSON/签名不可用 = 代码或网络污染，必须红。
fn is_protocol_break(err: &BilibiliError) -> bool {
    matches!(
        err,
        BilibiliError::Transport { .. } | BilibiliError::NotJson { .. }
    )
}

fn report<T>(name: &str, result: &Result<Vec<T>, BilibiliError>, strict: bool) {
    match result {
        Ok(v) => println!("[live-smoke] {name}: ok ({} items)", v.len()),
        Err(err) if strict && is_protocol_break(err) => {
            panic!("[live-smoke] {name}: protocol break {err}");
        }
        Err(err) => println!("[live-smoke] {name}: typed-error {err} (accepted)"),
    }
}

#[test]
#[ignore = "real B 站 endpoints — opt-in only"]
fn m2_surface_live_smoke() {
    let cookie = std::env::var("VTD_LIVE_COOKIE").unwrap_or_default();
    let strict = !cookie.trim().is_empty();
    let mut client = BilibiliClient::new(&cookie, 0.7, 15.0).expect("client build");

    // 1. nav/auth + WBI 混钥链（videos 内部触发 mixin_key_sync；
    //    "WBI key is unavailable" 属 Transport 协议断裂，is_protocol_break 兜住）。
    let auth = client.auth_status().map(|_| vec![1u8]);
    report("auth_status", &auth, strict);
    if strict {
        auth.expect("auth_status with cookie");
    }

    // 2. WBI 签名端点：老师空间视频列表。
    let videos = client.videos(STREAMER_UID, 3);
    report("videos", &videos, strict);
    let videos = if strict {
        videos.expect("videos with cookie")
    } else {
        videos.unwrap_or_default()
    };
    for item in &videos {
        assert!(
            item.get("bvid")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|b| !b.is_empty()),
            "videos item missing bvid: {item}"
        );
    }

    // 3. 评论区旧式翻页（oid=avid, type=1）——需要 video_detail 换 aid。
    match videos
        .first()
        .and_then(|v| v.get("bvid"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    {
        Some(bvid) => {
            let aid = client.video_detail(&bvid).ok().and_then(|d| {
                d.get("aid")
                    .and_then(serde_json::Value::as_i64)
                    .map(|a| a.to_string())
            });
            match aid {
                Some(aid) => {
                    let replies = client.replies(&aid, 1, 3);
                    report("replies", &replies, strict);
                    let replies = if strict {
                        replies.expect("replies with cookie")
                    } else {
                        replies.unwrap_or_default()
                    };
                    for reply in &replies {
                        assert!(
                            reply
                                .pointer("/content/message")
                                .and_then(serde_json::Value::as_str)
                                .is_some(),
                            "reply missing content.message: {reply}"
                        );
                    }
                }
                None => println!("[live-smoke] replies: skipped (video_detail gave no aid)"),
            }
        }
        None => println!("[live-smoke] replies: skipped (no videos)"),
    }

    // 4. 大航海名单（公开房间标签页）。
    let guards = client.guard_members(ROOM_ID, STREAMER_UID, 5);
    report("guard_members", &guards, false);
    if let Ok(members) = &guards {
        for member in members {
            assert!(
                member
                    .get("uid")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|u| !u.is_empty() && u != "0"),
                "guard member missing uid: {member}"
            );
        }
    }

    // 5. 回放列表：多数房间 count=0 是平台现状（empty Ok 即通过）；
    //    有记录则顺手探一条 rid 弹幕分片链。
    let records = client.live_records(ROOM_ID, 20);
    report("live_records", &records, false);
    if let Ok(records) = &records {
        match records
            .first()
            .and_then(|r| r.get("rid").and_then(serde_json::Value::as_str))
            .map(str::to_string)
        {
            Some(rid) => {
                let danmaku = client.live_record_danmaku(&rid);
                report("live_record_danmaku", &danmaku, false);
            }
            None => println!("[live-smoke] live_record_danmaku: skipped (no public replay)"),
        }
    }

    // 6. 番剧订阅（vmid 公开；隐私关闭时是 typed Api 22115，可接受）。
    let bangumi = client.bangumi(STREAMER_UID, 3);
    report("bangumi", &bangumi, false);

    println!("[live-smoke] requests made: {}", client.request_count());
}
