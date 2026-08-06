//! R2 批 2 D1「WS 弹幕窗」—— 协议 codec 层的合成字节流 fixture 钉（测试即规格）。
//!
//! 协议真理源：bilibili-API-collect `docs/live/message_stream.md`（执行单
//! 2026-08-06-r2-execution-spec.md 批 2 已核对；本文件按字节级/事件级/构造级三类钉死）。
//! 本文件是全新件，不触碰黄金样本 / parity 面（零行为打扰既有测试）。
//!
//! 包格式（全大端）：[0..4] u32 总长（头+body）|[4..6] u16 头长=16|[6..8] u16 版本
//! |[8..12] u32 操作码|[12..16] u32 序号=1|body。版本：0=明文 JSON|1=整数 body|2=zlib|3=brotli。

use std::io::Write;

use live_core::live_ws::codec::{
    CodecError, RawPacket, WS_FAILSAFE_CAP_HOURS, decode_packets, encode_packet, unpack,
};
use live_core::live_ws::message::{
    IgnoreTally, WsEvent, auth_packet_body, heartbeat_frame, parse_packet,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// 手工压缩 helper（fixture 与实现同源，双向验证）
// ---------------------------------------------------------------------------

fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).expect("zlib write");
    enc.finish().expect("zlib finish")
}

fn brotli_compress(data: &[u8]) -> Vec<u8> {
    let mut enc = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
    enc.write_all(data).expect("brotli write");
    enc.into_inner()
}

/// 按 op/version/body 组一个原生包。
fn p(op: u32, version: u16, body: &[u8]) -> RawPacket {
    RawPacket {
        op,
        version,
        body: body.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// 工程常量与头骨架钉
// ---------------------------------------------------------------------------

#[test]
fn failsafe_cap_hours_pinned() {
    // 工程保险丝常量（规格定值）。
    assert_eq!(WS_FAILSAFE_CAP_HOURS, 12.0);
}

#[test]
fn header_len_pinned_sixteen() {
    // HEADER_LEN 是协议定值（头 16 字节），本钉防误改线格式。
    assert_eq!(live_core::live_ws::codec::HEADER_LEN, 16);
}

#[test]
fn encode_packet_wires_full_header() {
    let frame = encode_packet(7, 0, b"xy");
    assert_eq!(frame.len(), 16 + 2);
    assert_eq!(u32::from_be_bytes(frame[0..4].try_into().unwrap()), 18);
    assert_eq!(u16::from_be_bytes(frame[4..6].try_into().unwrap()), 16);
    assert_eq!(u16::from_be_bytes(frame[6..8].try_into().unwrap()), 0);
    assert_eq!(u32::from_be_bytes(frame[8..12].try_into().unwrap()), 7);
    assert_eq!(u32::from_be_bytes(frame[12..16].try_into().unwrap()), 1);
    assert_eq!(&frame[16..], b"xy");
}

// ---------------------------------------------------------------------------
// 往返钉：encode → decode 原样回来（含多包拼接流收齐）
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_single_json_packet() {
    let body = r#"{"cmd":"LIVE","live_time":1787654321}"#.as_bytes();
    let frame = encode_packet(5, 0, body);
    let out = decode_packets(&frame).expect("decode");
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].op, out[0].version), (5, 0));
    assert_eq!(&out[0].body[..], body);
}

#[test]
fn roundtrip_binary_body_packet() {
    // 二进制 body（u32 人气值）同样原样收放。
    let raw = 987654u32.to_be_bytes();
    let frame = encode_packet(3, 1, &raw);
    let out = decode_packets(&frame).expect("decode");
    assert_eq!(out.len(), 1);
    assert_eq!(&out[0].body[..], &raw[..]);
}

#[test]
fn roundtrip_multi_packet_stream() {
    // 4 包连续流：一次喂入全部收齐、顺序与字节一致。
    let p1 = encode_packet(7, 0, r#"{"code":0}"#.as_bytes());
    let p2 = encode_packet(3, 1, &42u32.to_be_bytes());
    let p3 = encode_packet(5, 0, r#"{"cmd":"LIVE"}"#.as_bytes());
    let p4 = encode_packet(2, 1, b"[object Object]");
    let stream = [p1.as_slice(), p2.as_slice(), p3.as_slice(), p4.as_slice()].concat();

    let out = decode_packets(&stream).expect("decode multi");
    assert_eq!(out.len(), 4);
    assert_eq!((out[0].op, out[0].version), (7, 0));
    assert_eq!((out[1].op, out[1].version), (3, 1));
    assert_eq!((out[2].op, out[2].version), (5, 0));
    assert_eq!((out[3].op, out[3].version), (2, 1));
    assert_eq!(&out[3].body[..], b"[object Object]");
}

#[test]
fn empty_body_packet_is_legal() {
    // total == HEADER_LEN（16）即零 body 包，属于合法形状。
    let frame = encode_packet(5, 0, b"");
    let out = decode_packets(&frame).expect("decode empty");
    assert_eq!(out.len(), 1);
    assert!(out[0].body.is_empty());
}

// ---------------------------------------------------------------------------
// 截断三态钉（绝不静默对齐）
// ---------------------------------------------------------------------------

#[test]
fn truncation_header_under_sixteen() {
    // 余量不足 16 字节，整头切不出 → TruncatedHeader。
    let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let err = decode_packets(&bytes).expect_err("must err");
    assert!(matches!(err, CodecError::TruncatedHeader));
}

#[test]
fn truncation_body_under_declared_total() {
    // 头部声明 total=8（<16，连头长都填不满）→ TruncatedBody。
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&8u32.to_be_bytes());
    let err = decode_packets(&bytes).expect_err("must err");
    assert!(matches!(err, CodecError::TruncatedBody));
}

#[test]
fn truncation_total_overstates_available() {
    // 总长字声称 64，实际可读只有 20 → TruncatedLen。
    let stream = {
        let mut s = encode_packet(5, 0, b"1234");
        s[0..4].copy_from_slice(&64u32.to_be_bytes());
        s
    };
    let err = decode_packets(&stream).expect_err("must err");
    assert!(matches!(err, CodecError::TruncatedLen));
}

#[test]
fn truncation_partial_cut_at_boundary() {
    // 齐包 + 半截头尾缀：不该错收下一个包 → TruncatedHeader。
    let mut stream = encode_packet(5, 0, b"ok");
    stream.extend_from_slice(&[0u8; 7]);
    let err = decode_packets(&stream).expect_err("must err");
    assert!(matches!(err, CodecError::TruncatedHeader));
}

// ---------------------------------------------------------------------------
// 版本 0/1 直通 + 版本 2/3 解压臂（多包串联内流）
// ---------------------------------------------------------------------------

#[test]
fn plain_versions_pass_through() {
    for (version, body) in [(0u16, "{}"), (1u16, "[object Object]")] {
        let raw = p(5, version, body.as_bytes());
        let out = unpack(&raw).expect("plain");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], raw);
    }
}

#[test]
fn zlib_arm_expands_inner_stream() {
    let inner_auth = encode_packet(8, 0, r#"{"code":0}"#.as_bytes());
    let inner_dan = encode_packet(
        5,
        0,
        r#"{"cmd":"DANMU_MSG","info":[0,"联赛三分",[42,"阿冰"]]}"#.as_bytes(),
    );
    let stream = [inner_auth.as_slice(), inner_dan.as_slice()].concat();
    let blob = zlib_compress(&stream);

    let outer = p(5, 2, &blob);
    let inner = unpack(&outer).expect("zlib unpack");
    assert_eq!(inner.len(), 2);
    assert_eq!(inner[0].op, 8);
    assert_eq!(inner[1].op, 5);

    assert_eq!(
        parse_packet(&inner[0]).expect("parse").expect("some"),
        WsEvent::AuthAck { code: 0 }
    );
    assert_eq!(
        parse_packet(&inner[1]).expect("parse").expect("some"),
        WsEvent::Danmaku {
            uid: "42".into(),
            uname: "阿冰".into(),
            text: "联赛三分".into()
        }
    );
}

#[test]
fn brotli_arm_expands_inner_stream() {
    let inner_a = encode_packet(8, 0, r#"{"code":0}"#.as_bytes());
    let inner_live = encode_packet(5, 0, r#"{"cmd":"LIVE","live_time":1750000000}"#.as_bytes());
    let stream = [inner_a.as_slice(), inner_live.as_slice()].concat();
    let blob = brotli_compress(&stream);

    let outer = p(5, 3, &blob);
    let inner = unpack(&outer).expect("brotli unpack");
    assert_eq!(inner.len(), 2);
    assert_eq!(
        parse_packet(&inner[0]).expect("parse").expect("some"),
        WsEvent::AuthAck { code: 0 }
    );
    assert_eq!(
        parse_packet(&inner[1]).expect("parse").expect("some"),
        WsEvent::Live {
            live_time: 1750000000
        }
    );
}

#[test]
fn unknown_version_rejected() {
    let raw = p(5, 9, b"");
    let err = unpack(&raw).expect_err("bad version");
    assert!(matches!(err, CodecError::BadVersion(9)));
}

#[test]
fn corrupt_deflate_stream_errors_inflate() {
    let raw = p(5, 2, b"definitely-not-zlib");
    let err = unpack(&raw).expect_err("corrupt");
    assert!(matches!(err, CodecError::Inflate(_)));
}

// ---------------------------------------------------------------------------
// 消息族钉（op=5 下行 JSON）
// ---------------------------------------------------------------------------

#[test]
fn danmaku_standard_body_fields() {
    let raw = p(
        5,
        0,
        r#"{"cmd":"DANMU_MSG","info":[0,"我再打一把就睡",[3546595083686995,"苏夏陈树"]]}"#
            .as_bytes(),
    );
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::Danmaku {
            uid: "3546595083686995".to_string(),
            uname: "苏夏陈树".to_string(),
            text: "我再打一把就睡".to_string(),
        }
    );
}

#[test]
fn danmaku_missing_info_sections_dropped() {
    // 缺 info[1]（无文本）→ Ok(None)，绝不补段。
    let no_text = p(
        5,
        0,
        r#"{"cmd":"DANMU_MSG","info":[[],[42,"阿冰"]]}"#.as_bytes(),
    );
    assert!(parse_packet(&no_text).expect("parse").is_none());
    // 缺 info[2]（无发者身份）→ Ok(None)。
    let no_user = p(5, 0, r#"{"cmd":"DANMU_MSG","info":[0,"文本"]}"#.as_bytes());
    assert!(parse_packet(&no_user).expect("parse").is_none());
    // uid 非数值 → Ok(None)。
    let no_uid = p(
        5,
        0,
        r#"{"cmd":"DANMU_MSG","info":[0,"文本",["x"]]}"#.as_bytes(),
    );
    assert!(parse_packet(&no_uid).expect("parse").is_none());
}

#[test]
fn super_chat_message_fields() {
    let raw = p(
        5,
        0,
        r#"{"cmd":"SUPER_CHAT_MESSAGE","data":{"price":300,"message":"谢谢老板","start_time":1700000001,"user_info":{"uid":10,"uname":"金主"}}}"#.as_bytes(),
    );
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::SuperChat {
            uid: "10".into(),
            uname: "金主".into(),
            text: "谢谢老板".into(),
            price: 300.0,
            start_time: Some(1700000001)
        }
    );
}

#[test]
fn super_chat_message_jpn_same_shape() {
    // 平台 `start_time` 缺席时段位留 None——绝不自造时间（SC ts 回落本地受时在第二段）。
    let raw = p(
        5,
        0,
        r#"{"cmd":"SUPER_CHAT_MESSAGE_JPN","data":{"price":100,"message":"こんにちは","user_info":{"uid":99,"uname":"jpn"}}}"#.as_bytes(),
    );
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::SuperChat {
            uid: "99".into(),
            uname: "jpn".into(),
            text: "こんにちは".into(),
            price: 100.0,
            start_time: None
        }
    );
}

#[test]
fn super_chat_delete_ids_collected() {
    let raw = p(
        5,
        0,
        r#"{"cmd":"SUPER_CHAT_MESSAGE_DELETE","data":{"ids":[123,456]}}"#.as_bytes(),
    );
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::SuperChatDelete {
            ids: vec![123, 456]
        }
    );
}

#[test]
fn super_chat_delete_empty_ids_dropped() {
    // data.ids 空数组：没有可撤销项 → Ok(None)。
    let raw = p(
        5,
        0,
        r#"{"cmd":"SUPER_CHAT_MESSAGE_DELETE","data":{"ids":[]}}"#.as_bytes(),
    );
    assert!(parse_packet(&raw).expect("parse").is_none());
}

#[test]
fn interact_word_kind1_enter() {
    let raw = p(
        5,
        0,
        r#"{"cmd":"INTERACT_WORD","data":{"msg_type":1,"uid":313,"uname":"路人","timestamp":1700000000}}"#.as_bytes(),
    );
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::Interact {
            kind: 1,
            uid: "313".into(),
            uname: "路人".into(),
            ts: 1700000000
        }
    );
}

#[test]
fn interact_word_unknown_msg_type_verbatim() {
    // 未知 msg_type（7）：值原样登记，绝不猜语义。
    let raw = p(
        5,
        0,
        r#"{"cmd":"INTERACT_WORD","data":{"msg_type":7,"uid":1,"uname":"?","timestamp":0}}"#
            .as_bytes(),
    );
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::Interact {
            kind: 7,
            uid: "1".into(),
            uname: "?".into(),
            ts: 0
        }
    );
}

#[test]
fn interact_word_v2_parked_none_but_tallied() {
    // INTERACT_WORD_V2（base64 protobuf）本轮登记不解析：Ok(None) 且 IgnoreTally 可见。
    let raw = p(
        5,
        0,
        r#"{"cmd":"INTERACT_WORD_V2","data":"hAeBARKB"}"#.as_bytes(),
    );
    assert!(parse_packet(&raw).expect("parse").is_none());
    let tally = IgnoreTally::new();
    tally.record("INTERACT_WORD_V2");
    tally.record("INTERACT_WORD_V2");
    assert_eq!(tally.snapshot()["INTERACT_WORD_V2"], 2);
}

#[test]
fn live_signal_carries_live_time() {
    let raw = p(5, 0, r#"{"cmd":"LIVE","live_time":1750000000}"#.as_bytes());
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::Live {
            live_time: 1750000000
        }
    );
}

#[test]
fn preparing_round1_treated_as_shutdown() {
    let raw = p(5, 0, r#"{"cmd":"PREPARING","round":1}"#.as_bytes());
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::Preparing { round: 1 }
    );
}

#[test]
fn unknown_cmd_returns_none_and_talliable() {
    let raw = p(5, 0, r#"{"cmd":"FUTURE_UNKNOWN"}"#.as_bytes());
    assert!(parse_packet(&raw).expect("parse").is_none());
    let tally = IgnoreTally::new();
    tally.record("FUTURE_UNKNOWN");
    assert_eq!(tally.snapshot()["FUTURE_UNKNOWN"], 1);
}

#[test]
fn bad_json_errors() {
    let raw = p(5, 0, br#"{"cmd": broken"#);
    assert!(matches!(
        parse_packet(&raw).expect_err("bad json"),
        CodecError::BadJson(_)
    ));
}

#[test]
fn non_object_json_is_none() {
    // JSON 合法但非对象（数组 body，无 cmd）→ Ok(None)。
    let raw = p(5, 0, b"[1,2,3]");
    assert!(parse_packet(&raw).expect("parse").is_none());
}

// ---------------------------------------------------------------------------
// op=3 人气 / op=8 认证回复钉
// ---------------------------------------------------------------------------

#[test]
fn heartbeat_reply_popularity_u32_be() {
    let raw = p(3, 1, &987654u32.to_be_bytes());
    assert_eq!(
        parse_packet(&raw).expect("parse").expect("some"),
        WsEvent::Popularity { value: 987654 }
    );
}

#[test]
fn heartbeat_reply_short_body_errors_truncated() {
    let raw = p(3, 1, &[1, 2, 3]);
    assert!(matches!(
        parse_packet(&raw).expect_err("short body"),
        CodecError::TruncatedBody
    ));
}

#[test]
fn auth_ack_code_zero_and_negative() {
    let ok = p(8, 0, r#"{"code":0,"message":""}"#.as_bytes());
    assert_eq!(
        parse_packet(&ok).expect("parse").expect("some"),
        WsEvent::AuthAck { code: 0 }
    );
    let refused = p(8, 0, r#"{"code":-101}"#.as_bytes());
    assert_eq!(
        parse_packet(&refused).expect("parse").expect("some"),
        WsEvent::AuthAck { code: -101 }
    );
}

// ---------------------------------------------------------------------------
// 认证 / 心跳构造器钉
// ---------------------------------------------------------------------------

#[test]
fn auth_packet_body_has_all_spec_fields() {
    let body = auth_packet_body(1_790_370_612, 0, "5f1d…token");
    let v: Value = serde_json::from_slice(&body).expect("auth body json");
    assert_eq!(v["uid"], 0);
    assert_eq!(v["roomid"], 1_790_370_612);
    assert_eq!(v["uid"], 0);
    assert_eq!(v["protover"], 3);
    assert_eq!(v["key"], "5f1d…token");
}

#[test]
fn heartbeat_frame_shape() {
    // 头 [0..4] 总长 = 31；[4..6] 头长 = 16；[6..8] 版本 = 1；[8..12] op = 2；[12..16] seq = 1；
    // body = 字节串 "[object Object]"（15 字节），故整帧 = 16 + 15 = 31 字节。
    // 备注：执行单批 2 的「31 字节」指整帧总长（16 头 + 15 body）而非 body 单独长度。
    let frame = heartbeat_frame();
    assert_eq!(frame.len(), 31);
    assert_eq!(u32::from_be_bytes(frame[0..4].try_into().unwrap()), 31);
    assert_eq!(u16::from_be_bytes(frame[4..6].try_into().unwrap()), 16);
    assert_eq!(u16::from_be_bytes(frame[6..8].try_into().unwrap()), 1);
    assert_eq!(u32::from_be_bytes(frame[8..12].try_into().unwrap()), 2);
    assert_eq!(u32::from_be_bytes(frame[12..16].try_into().unwrap()), 1);
    assert_eq!(&frame[16..], b"[object Object]");
}
