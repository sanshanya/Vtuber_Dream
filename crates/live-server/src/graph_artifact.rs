//! 整体图谱 payload 物化 + 协商服务（弱指针，内容寻址）。
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
//! 内容模型（G2-E 盲评立单修复）：图面**并非纯 append-only**——
//! upsert_edge 原地合入（RELATED_TO interpretation / INTERESTED_IN evidence+confidence
//! 跨 run 变化）、entity_merge 重指坐标（source/target 变而 valid_to 不动）、upsert_node
//! 改名——三条原地写路径若逃逸指纹，AI 重跑/维护合并后物化将静默服旧字节。
//! 探针列集 =「cytoscape payload 可见列」+「project SQL 过滤臂谓词列」的并集：
//! nodes(node_id,node_type,name,properties_json) /
//! edges(edge_id,source,target,predicate,confidence,evidence_json,properties_json,
//!       source_kind,run_id,valid_to)；
//! valid_from/first_seen_at/last_seen_at/viewer_id 为装饰列（不进 DTO 不进过滤臂）——
//! 拿掉它们的代价是主理会话 rerun 后 etag 必翻但 payload 不变的「如实幂等」仍有：
//! run_id 随活跃边换代是写面语义（新 run 的新活跃集 = 新内容身份），立此为据。

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

/// 折叠/出图算法版本串：任何「elements 产出规则」改变必须翻版本，否则外置物化
/// 会静默服旧算法的字节（2026-08-05 实锤：零度散点裁除落地后，旧 artifact 因
/// 指纹不含算法版本继续吐 2522 节点的岩浆版）。
pub const GRAPH_FOLD_VERSION: &str = "fold-v2-zero-degree-drop-2026-08-05";

/// 探针：投影可见内容 + 白名单 + **算法版本**的 sha256 前缀 = 指纹 = ETag。
/// 列集拟定依据 =「DTO 可见列 ∪ project SQL 过滤臂谓词列」（卷首注 G2-E）。
pub fn content_probe(store: &Store, kinds_csv: &str, fold_version: &str) -> StoreResult<String> {
    let mut hasher = sha2::Sha256::new();
    {
        let mut stmt = store.conn.prepare(
            "SELECT node_id || char(31) || node_type || char(31) || name || char(31) \
             || properties_json FROM nodes ORDER BY node_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        hash_rows(
            &mut rows.map(|row| row.map_err(StoreError::from)),
            &mut hasher,
        )?;
    }
    {
        let mut stmt = store.conn.prepare(
            "SELECT edge_id || char(31) || source_id || char(31) || target_id || char(31) \
             || predicate || char(31) || coalesce(cast(confidence as text), '') || char(31) \
             || evidence_json || char(31) || properties_json || char(31) || source_kind \
             || char(31) || coalesce(run_id, '') || char(31) || coalesce(valid_to, '') \
             FROM edges ORDER BY edge_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        hash_rows(
            &mut rows.map(|row| row.map_err(StoreError::from)),
            &mut hasher,
        )?;
    }
    hasher.update(kinds_csv.as_bytes());
    hasher.update([0]);
    hasher.update(fold_version.as_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-08-05 实锤钉：算法迭代必须翻面指纹（v1 零度岩浆版事故）——
    /// 同库同白名单，版本串变 = 探针输出变；同版本 = 输出不变。
    #[test]
    fn content_probe_is_sensitive_to_fold_version() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("probe.sqlite3")).unwrap();
        let v2 = content_probe(
            &store,
            "Viewer,Entity",
            "fold-v2-zero-degree-drop-2026-08-05",
        )
        .unwrap();
        let v2_again = content_probe(
            &store,
            "Viewer,Entity",
            "fold-v2-zero-degree-drop-2026-08-05",
        )
        .unwrap();
        let v3 = content_probe(&store, "Viewer,Entity", "fold-v3-next-algorithm").unwrap();
        assert_eq!(v2, v2_again, "同名指纹必须稳定");
        assert_ne!(v2, v3, "版本串必须进指纹");
    }
}
