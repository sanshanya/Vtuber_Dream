//! Z6/P0-6：整体图谱 payload 物化 + 协商服务（弱指针，内容寻址）。
//!
//! 现状痛点（s0 实测）：`/api/rooms/{id}/graph` 每次重跑 project()（≈0.6s CPU）并吐
//! 1.92MB raw JSON、零压缩、零缓存协商。
//!
//! 失效信号选型（misfire 记录在册）：**mtime / sqlite change counter 均不可用**——
//! 本库所有读端点在 Store::open+close（WAL 落账）都会翻页这两个头字段（实测
//! 179→180→181→182/request），以它们作指纹会使每请求重建、自激振荡。正解 =
//! **内容探针哈希**：两列 ORDER BY 流式扫描（nodes/edges 身份键+负载）+ 白名单 csv
//! 经 sha256 出 16hex —— 既是失效指纹，又是内容寻址 ETag（payload 是行集的确定性
//! 派生物）。探针一次读扫 ≈数 MB、毫秒级；若内容零变化，读面命中直接服务
//! `web-graph.<etag>.json{,.gz,.br}` 三件套，零 project 重算。
//!
//! 内容模型注意：图面 append-only + 关闭态整行进投影（valid_to 是投影内容的一部分），
//! 故探针覆盖 (node_id, properties_json) 与 (edge_id, predicate, valid_to COALESCE) 即
//! 足以界定「投影可见内容」。

use std::io::Read;

use live_core::graph::store::{Result as StoreResult, Store, StoreError};
use sha2::Digest;

/// 小于此字节数的 payload 不值得开压缩通道（与 vite-plugin-compression threshold 对齐）。
pub const ARTIFACT_COMPRESS_MIN_BYTES: usize = 1024;
/// brotli 建物档质量：离线重建，按 CPU 换字节取 5（cytoscape JSON 实测 br ≈ raw × 19%）。
pub const ARTIFACT_BROTLI_QUALITY: u32 = 5;
/// 内容寻址指纹/ETag 长度（stable_hash 家族前缀惯例 16hex）。
pub const ARTIFACT_ETAG_HEX: usize = 16;

#[derive(Debug)]
pub struct GraphArtifact {
    pub etag: String,
    pub raw: Vec<u8>,
    pub gz: Option<Vec<u8>>,
    pub br: Option<Vec<u8>>,
}

/// 行集 → sha256 流（确定性序 = 调用方 SQL ORDER BY 负责）。
fn hash_rows(
    rows: &mut dyn Iterator<Item = StoreResult<String>>,
    hasher: &mut sha2::Sha256,
) -> StoreResult<()> {
    for row in rows {
        hasher.update(row?.as_bytes());
        hasher.update([0]);
    }
    Ok(())
}

/// 探针：投影可见内容 + 白名单的 sha256 前缀 = 指纹 = ETag。
pub fn content_probe(store: &Store, kinds_csv: &str) -> StoreResult<String> {
    let mut hasher = sha2::Sha256::new();
    {
        let mut stmt = store
            .conn
            .prepare("SELECT node_id || char(31) || properties_json FROM nodes ORDER BY node_id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        hash_rows(
            &mut rows.map(|row| row.map_err(StoreError::from)),
            &mut hasher,
        )?;
    }
    {
        let mut stmt = store.conn.prepare(
            "SELECT edge_id || char(31) || predicate || char(31) || coalesce(valid_to, '') \
             FROM edges ORDER BY edge_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        hash_rows(
            &mut rows.map(|row| row.map_err(StoreError::from)),
            &mut hasher,
        )?;
    }
    hasher.update(kinds_csv.as_bytes());
    Ok(hex16(&hasher.finalize()))
}

fn hex16(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(ARTIFACT_ETAG_HEX / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn artifact_paths(
    root: &std::path::Path,
    etag: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let dir = root.join("graph");
    (
        dir.join(format!("web-graph.{etag}.json")),
        dir.join(format!("web-graph.{etag}.json.gz")),
        dir.join(format!("web-graph.{etag}.json.br")),
    )
}

fn compress_gzip(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut encoder, raw)?;
    encoder.finish()
}

fn compress_brotli(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut reader = brotli::CompressorReader::new(raw, 4096, ARTIFACT_BROTLI_QUALITY, 22);
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

/// 建物：序列化 → 压缩 → tmp+rename 原子落档每通道 → 清扫旧 etag 残档。
pub fn write_artifact(
    root: &std::path::Path,
    etag: &str,
    raw_json: &str,
) -> std::io::Result<GraphArtifact> {
    let raw = raw_json.as_bytes().to_vec();
    let artifact = GraphArtifact {
        gz: (raw.len() >= ARTIFACT_COMPRESS_MIN_BYTES)
            .then(|| compress_gzip(&raw))
            .transpose()?,
        br: (raw.len() >= ARTIFACT_COMPRESS_MIN_BYTES)
            .then(|| compress_brotli(&raw))
            .transpose()?,
        raw,
        etag: etag.to_string(),
    };
    let (raw_path, gz_path, br_path) = artifact_paths(root, etag);
    let write_atomic = |path: &std::path::Path, bytes: &[u8]| -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    };
    write_atomic(&raw_path, &artifact.raw)?;
    match &artifact.gz {
        Some(bytes) => write_atomic(&gz_path, bytes)?,
        None => {
            let _ = std::fs::remove_file(&gz_path);
        }
    }
    match &artifact.br {
        Some(bytes) => write_atomic(&br_path, bytes)?,
        None => {
            let _ = std::fs::remove_file(&br_path);
        }
    }
    // 机会 GC：清掉其他 etag 的旧三件套（后写胜；并发双重建时残留最坏 = 多一档）。
    if let Ok(entries) = std::fs::read_dir(root.join("graph")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("web-graph.") && !name.contains(&format!(".{etag}.")) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(artifact)
}

/// 读面：三件套齐 → GraphArtifact；缺一即 None（走重建）。
pub fn read_artifact(root: &std::path::Path, etag: &str) -> Option<GraphArtifact> {
    let (raw_path, gz_path, br_path) = artifact_paths(root, etag);
    let raw = std::fs::read(raw_path).ok()?;
    Some(GraphArtifact {
        gz: std::fs::read(gz_path).ok(),
        br: std::fs::read(br_path).ok(),
        raw,
        etag: etag.to_string(),
    })
}
