//! 字节层：16 字节大端头切包/组装，版本 2/3 解压后的多包串联流展开。
//!
//! 协议真理源：bilibili-API-collect `docs/live/message_stream.md`（执行单
//! 2026-08-06 已核对）。合成字节流 fixture 即规格：本层全部验收钉在
//! `crates/live-core/tests/live_ws_codec.rs`，模块内不另设重复夹具。
//!
//! 线格式（全大端）：
//!   [0..4]   u32 整包总长（头 16 + body）
//!   [4..6]   u16 头长（恒 16）
//!   [6..8]   u16 协议版本：0=明文 JSON；1=整数 body（心跳/认证）；2=zlib；3=brotli
//!   [8..12]  u32 操作码：2=客户端心跳；3=心跳回复；5=下行；7=认证；8=认证回复
//!   [12..16] u32 序号（恒 1）
//!
//! 版本 2/3：body 是**压缩的多包串联流**——解压后内部仍有完整 16 字节头，
//! 必须按内部头总长迭代逐个切出（见 `unpack`）。
//!
//! 截断三态裁决（绝不静默对齐、绝不错包续读）：
//!   - 余量不足 16 字节、连头都切不出             → `TruncatedHeader`
//!   - 总长字段声称的总长超出可读余量（总长字说大了） → `TruncatedLen`
//!   - 总长字段连 16 字节头长都填不满（体长不可能成立）→ `TruncatedBody`

use std::io::Read;

/// 16 字节大端头长（协议定值）。
pub const HEADER_LEN: usize = 16;

/// 工程保险丝：WS 全链路失联后允许悬挂的最长小时数。
/// 规格定值、本段先立位；接上 run 侧时机判断是第二段的活。
pub const WS_FAILSAFE_CAP_HOURS: f64 = 12.0;

/// 操作码（协议定值）。认证是 op=7，本段只给出 body 构造（message::auth_packet_body），
/// 帧头由第二段用 `encode_packet(7, 0, body)` 组装，故不单独设常量。
pub(crate) const OP_HEARTBEAT_CLIENT: u32 = 2;
pub(crate) const OP_HEARTBEAT_REPLY: u32 = 3;
pub(crate) const OP_DOWNSTREAM: u32 = 5;
pub(crate) const OP_AUTH_REPLY: u32 = 8;

/// 协议版本（头 [6..8]）。
pub(crate) const VERSION_PLAIN_JSON: u16 = 0;
pub(crate) const VERSION_INTEGER_BODY: u16 = 1;
pub(crate) const VERSION_ZLIB: u16 = 2;
pub(crate) const VERSION_BROTLI: u16 = 3;

/// 一帧切出来的原生包：头部字段 + 未加工 body 字节。
/// 版本 2/3 的 body 记得先过 `unpack` 再进 `message::parse_packet`。
#[derive(Debug, Clone, PartialEq)]
pub struct RawPacket {
    pub version: u16,
    pub op: u32,
    pub body: Vec<u8>,
}

/// Codec 层错误。截断一律 Err、绝不静默对齐（三条截断态释义见文件头）。
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// 总长字段声称的包超出当前可读字节流（「总长字说大了」）。
    #[error("包被截断：总长字段声称的大小超出可读字节流")]
    TruncatedLen,
    /// 尾部不足 16 字节，整头都切不出来。
    #[error("包被截断：不足 16 字节头")]
    TruncatedHeader,
    /// 总长字段连 16 字节头长都填不满，body 段不可能成立。
    #[error("包被截断：总长字段连 16 字节头长都声明不满")]
    TruncatedBody,
    /// 头 [6..8] 声明了 0/1/2/3 之外的协议版本。
    #[error("协议版本 {0} 不支持")]
    BadVersion(u16),
    /// zlib/brotli 解压读取失败（第 2 段会话层按包级容错决定去留）。
    #[error("解压失败：{0}")]
    Inflate(String),
    /// JSON 载荷解析失败（op=5/8 的 body 不是合法 JSON）。
    #[error("JSON 载荷解析失败：{0}")]
    BadJson(String),
}

/// 合成一帧（16 字节头 + body）。无校验——协议版本的合法性由 `unpack` 判。
pub fn encode_packet(op: u32, version: u16, body: &[u8]) -> Vec<u8> {
    let total = HEADER_LEN + body.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(HEADER_LEN as u16).to_be_bytes());
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&op.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes()); // sequence 恒 1
    out.extend_from_slice(body);
    out
}

/// 连续字节流切包：头部背叛 16 字节可能多包；任何截断/畸形即 Err，
/// 绝不静默对齐、不猜下一个包的边界。
///
/// 截断三态判定顺序（与文件头一一对应）：
/// 1. 当前余量 < 16 → `TruncatedHeader`（头都切不出）；
/// 2. 总长字段 < 16 → `TruncatedBody`（声明连头长都填不满）；
/// 3. 总长字段 > 余量 → `TruncatedLen`（总长字说大了）。
pub fn decode_packets(bytes: &[u8]) -> Result<Vec<RawPacket>, CodecError> {
    let mut packets = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let remaining = bytes.len() - offset;
        if remaining < HEADER_LEN {
            return Err(CodecError::TruncatedHeader);
        }
        let total = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        if total < HEADER_LEN {
            return Err(CodecError::TruncatedBody);
        }
        if total > remaining {
            return Err(CodecError::TruncatedLen);
        }
        let version = u16::from_be_bytes([bytes[offset + 6], bytes[offset + 7]]);
        let op = u32::from_be_bytes([
            bytes[offset + 8],
            bytes[offset + 9],
            bytes[offset + 10],
            bytes[offset + 11],
        ]);
        let body = bytes[offset + HEADER_LEN..offset + total].to_vec();
        packets.push(RawPacket { version, op, body });
        offset += total;
    }
    Ok(packets)
}

/// 版本 2/3：解压 body 后再按内部头总长逐个切出（多包串联流）；0/1：原样单包。
/// 其他版本 → `BadVersion`。解压出来的内层流再走 `decode_packets` 同一套严格裁决。
pub fn unpack(packet: &RawPacket) -> Result<Vec<RawPacket>, CodecError> {
    match packet.version {
        VERSION_PLAIN_JSON | VERSION_INTEGER_BODY => Ok(vec![packet.clone()]),
        VERSION_ZLIB => {
            let mut decoder = flate2::read::ZlibDecoder::new(packet.body.as_slice());
            let mut inner = Vec::new();
            decoder
                .read_to_end(&mut inner)
                .map_err(|e| CodecError::Inflate(format!("zlib: {e}")))?;
            decode_packets(&inner)
        }
        VERSION_BROTLI => {
            let mut decoder = brotli::Decompressor::new(packet.body.as_slice(), 4096);
            let mut inner = Vec::new();
            decoder
                .read_to_end(&mut inner)
                .map_err(|e| CodecError::Inflate(format!("brotli: {e}")))?;
            decode_packets(&inner)
        }
        other => Err(CodecError::BadVersion(other)),
    }
}
