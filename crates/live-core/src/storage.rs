//! 小型 JSON 文件工具（移植 Python `storage.py`）。
//!
//! - `write_json`：父目录建齐 + 同名 `.tmp` 文件原子替换（中断不留半截 JSON）。
//! - `archive_current_snapshot`：采集前把现状复制到 `history/snapshots/<UTC戳>`；
//!   `graph/` 与 `history/` 永不归档、永不删除（长期时序记忆，AGENTS.md §7）。
//! - `reset_output`：清 viewers/site/shared/ai 与顶层 JSON，同样保留 graph/history。
//!
//! 错误一律显式返回（字符串形态），绝不静默（AGENTS.md §11）。

use std::path::{Path, PathBuf};

use serde_json::Value;

pub type StorageResult<T> = Result<T, String>;

pub const LEGACY_AGGREGATE_NAMES: [&str; 2] = ["analysis.json", "ai_analysis.json"];

fn io_err(context: &str, path: &Path, err: std::io::Error) -> String {
    format!("{context} {}: {err}", path.display())
}

/// Python `unlink(missing_ok=True)`：不存在不算错，其余错误显式抛。
fn remove_file_missing_ok(path: &Path) -> StorageResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_err("remove", path, err)),
    }
}

/// Python `read_json(path, default=None)`：不存在 → None；存在则必须是合法 JSON。
pub fn read_json(path: &Path) -> StorageResult<Option<Value>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).map_err(|err| io_err("read", path, err))?;
    let value = serde_json::from_str(&text)
        .map_err(|err| format!("invalid JSON {}: {err}", path.display()))?;
    Ok(Some(value))
}

/// Python `write_json`：indent=2、UTF-8 直写、`.tmp` 原子替换。
pub fn write_json(path: &Path, value: &Value) -> StorageResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io_err("create_dir_all", parent, err))?;
    }
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let temporary = PathBuf::from(tmp_name);
    let text = serde_json::to_string_pretty(value).map_err(|err| format!("serialize: {err}"))?;
    std::fs::write(&temporary, text).map_err(|err| io_err("write", &temporary, err))?;
    std::fs::rename(&temporary, path).map_err(|err| io_err("rename", path, err))?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> StorageResult<()> {
    std::fs::create_dir_all(target).map_err(|err| io_err("create_dir_all", target, err))?;
    let entries = std::fs::read_dir(source).map_err(|err| io_err("read_dir", source, err))?;
    for entry in entries {
        let entry = entry.map_err(|err| io_err("read_dir entry", source, err))?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|err| io_err("copy", &from, err))?;
        }
    }
    Ok(())
}

/// Python `archive_current_snapshot`：归档 collection.json/streamer.json/viewers/shared/ai；
/// 没有任何候选时返回 None；同秒冲突追加 `-1/-2/...` 后缀。
pub fn archive_current_snapshot(root: &Path) -> StorageResult<Option<PathBuf>> {
    for name in LEGACY_AGGREGATE_NAMES {
        remove_file_missing_ok(&root.join(name))?;
    }
    let candidates = [
        "collection.json",
        "streamer.json",
        "viewers",
        "shared",
        "ai",
    ]
    .iter()
    .map(|name| root.join(name))
    .collect::<Vec<_>>();
    if !candidates.iter().any(|path| path.exists()) {
        return Ok(None);
    }
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut target = root.join("history").join("snapshots").join(&stamp);
    let mut suffix = 1;
    while target.exists() {
        target = root
            .join("history")
            .join("snapshots")
            .join(format!("{stamp}-{suffix}"));
        suffix += 1;
    }
    std::fs::create_dir_all(&target).map_err(|err| io_err("create_dir_all", &target, err))?;
    for path in &candidates {
        if !path.exists() {
            continue;
        }
        let destination = target.join(path.file_name().unwrap_or_default());
        if path.is_dir() {
            copy_dir_recursive(path, &destination)?;
        } else {
            std::fs::copy(path, &destination).map_err(|err| io_err("copy", path, err))?;
        }
    }
    Ok(Some(target))
}

/// Python `reset_output`：清出下轮采集的工作面；graph/history 是长期记忆，不动。
pub fn reset_output(root: &Path) -> StorageResult<()> {
    std::fs::create_dir_all(root).map_err(|err| io_err("create_dir_all", root, err))?;
    for name in ["viewers", "site", "shared", "ai"] {
        let path = root.join(name);
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|err| io_err("remove_dir_all", &path, err))?;
        }
    }
    for name in ["collection.json", "streamer.json"]
        .iter()
        .chain(LEGACY_AGGREGATE_NAMES.iter())
    {
        remove_file_missing_ok(&root.join(name))?;
    }
    Ok(())
}

/// Python `load_viewers`：viewers/*.json、文件名排序、只收 JSON 对象；
/// 坏 JSON 显式报错（Python 行为：json.loads 抛异常）。
pub fn load_viewers(root: &Path) -> StorageResult<Vec<Value>> {
    let directory = root.join("viewers");
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = std::fs::read_dir(&directory)
        .map_err(|err| io_err("read_dir", &directory, err))?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension().and_then(|e| e.to_str()) == Some("json")).then_some(path)
        })
        .collect::<Vec<_>>();
    names.sort();
    let mut rows = Vec::new();
    for path in names {
        if let Some(value @ Value::Object(_)) = read_json(&path)? {
            rows.push(value);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn write_read_roundtrip_creates_parent_dirs() {
        let root = temp_root();
        let path = root.path().join("shared").join("deep").join("x.json");
        write_json(&path, &json!({"a": 1, "中文": "值"})).unwrap();
        let back = read_json(&path).unwrap().unwrap();
        assert_eq!(back, json!({"a": 1, "中文": "值"}));
        assert!(!path.with_file_name("x.json.tmp").exists());
    }

    #[test]
    fn read_json_missing_is_none_and_bad_json_is_error() {
        let root = temp_root();
        assert_eq!(read_json(&root.path().join("nope.json")).unwrap(), None);
        let bad = root.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(read_json(&bad).unwrap_err().contains("invalid JSON"));
    }

    #[test]
    fn reset_output_clears_work_surface_but_keeps_memory() {
        let root = temp_root();
        let root = root.path();
        for dir in ["viewers", "site", "shared", "ai", "graph", "history"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("keep.json"), "{}").unwrap();
        }
        write_json(&root.join("collection.json"), &json!({})).unwrap();
        write_json(&root.join("streamer.json"), &json!({})).unwrap();
        write_json(&root.join("analysis.json"), &json!({})).unwrap();

        reset_output(root).unwrap();

        for dir in ["viewers", "site", "shared", "ai"] {
            assert!(!root.join(dir).exists(), "{dir} should be removed");
        }
        for dir in ["graph", "history"] {
            assert!(root.join(dir).join("keep.json").exists(), "{dir} preserved");
        }
        assert!(!root.join("collection.json").exists());
        assert!(!root.join("streamer.json").exists());
        assert!(!root.join("analysis.json").exists());
    }

    /// MXA-11（r7）：leads.jsonl 是 M4.x 线索账本——在 reset_output 白名单**外**，
    /// 必须永不被清（工作面清理不碰长期账本；「存在性钉」防改清单时灭账）。
    #[test]
    fn reset_output_never_sweeps_leads_ledger() {
        let root = temp_root();
        let root = root.path();
        std::fs::write(root.join("leads.jsonl"), "{\"dedupe_key\":\"x\"}\n").unwrap();
        reset_output(root).unwrap();
        assert!(root.join("leads.jsonl").exists(), "账本必须在 reset 中存活");
    }

    #[test]
    fn archive_copies_candidates_with_unique_names() {
        let root = temp_root();
        let root = root.path();
        write_json(
            &root.join("viewers").join("1001.json"),
            &json!({"viewer": 1}),
        )
        .unwrap();
        write_json(&root.join("streamer.json"), &json!({"s": 1})).unwrap();

        let first = archive_current_snapshot(root)
            .unwrap()
            .expect("archive created");
        assert!(first.join("viewers").join("1001.json").exists());
        assert!(first.join("streamer.json").exists());
        // 图库内容即使存在也绝不进归档
        write_json(&root.join("graph").join("store.db"), &json!({})).unwrap();
        write_json(&root.join("streamer.json"), &json!({"s": 2})).unwrap();
        let second = archive_current_snapshot(root)
            .unwrap()
            .expect("second archive");
        assert_ne!(first, second);
        assert!(second.join("streamer.json").exists());
        assert!(!second.join("graph").exists());
    }

    #[test]
    fn archive_empty_root_returns_none() {
        let root = temp_root();
        assert_eq!(archive_current_snapshot(root.path()).unwrap(), None);
    }

    #[test]
    fn load_viewers_sorted_and_skips_non_objects() {
        let root = temp_root();
        let root = root.path();
        assert_eq!(load_viewers(root).unwrap(), Vec::<Value>::new());
        write_json(
            &root.join("viewers").join("b.json"),
            &json!({"viewer": {"id": "2"}}),
        )
        .unwrap();
        write_json(
            &root.join("viewers").join("a.json"),
            &json!({"viewer": {"id": "1"}}),
        )
        .unwrap();
        write_json(&root.join("viewers").join("c.json"), &json!([1, 2])).unwrap();
        std::fs::write(root.join("viewers").join("note.txt"), "skip").unwrap();
        let ids: Vec<String> = load_viewers(root)
            .unwrap()
            .iter()
            .map(|v| v["viewer"]["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["1".to_string(), "2".to_string()]);
    }
}
