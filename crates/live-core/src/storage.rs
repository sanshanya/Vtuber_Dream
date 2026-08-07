//! 小型 JSON 文件工具（移植 Python `storage.py`）。
//!
//! - `write_json`：父目录建齐 + 同名 `.tmp` 文件原子替换（中断不留半截 JSON）。
//! - `archive_current_snapshot`：采集前把现状复制到 `history/snapshots/<UTC戳>`；
//!   `graph/` 与 `history/` 永不归档、永不删除（长期时序记忆，AGENTS.md 时序优先）；
//!   `ai/` 同样不归档不下刀（重采保 AI——认知层由 input_hash 失效驱动，不归档即
//!   不复制冗余）。
//! - `reset_output`：只清事实面（viewers/site/shared 与顶层 JSON）；graph/history/ai
//!   三面永不下刀（长期记忆 + 认知层缓存各自续命，失效由 input_hash 判定，不靠山刀）。
//!
//! 错误一律显式返回（字符串形态），绝不静默（AGENTS.md 完成定义）。

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

/// Python `write_json`：indent=2、UTF-8 直写、tmp 原子替换。
/// tmp 名唯一化（pid+进程内序号，对齐 server 写键的 `.live-server-tmp-{pid}`
/// 约定）——固定 `<file>.tmp` 在同路径并发写时会共享 tmp，先 rename 的一方把另一方的
/// tmp 挪走，后者必炸 NotFound（两个 live-audience 进程同指一 output_dir 是真实场景；
/// 单进程内经 Registry 409 互斥）。
pub fn write_json(path: &Path, value: &Value) -> StorageResult<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io_err("create_dir_all", parent, err))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data");
    let temporary = path.with_file_name(format!(
        ".{file_name}.live-core-tmp-{}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let text = serde_json::to_string_pretty(value).map_err(|err| format!("serialize: {err}"))?;
    if let Err(err) = std::fs::write(&temporary, text) {
        let _ = std::fs::remove_file(&temporary);
        return Err(io_err("write", &temporary, err));
    }
    if let Err(err) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(io_err("rename", path, err));
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> StorageResult<()> {
    std::fs::create_dir_all(target).map_err(|err| io_err("create_dir_all", target, err))?;
    let entries = std::fs::read_dir(source).map_err(|err| io_err("read_dir", source, err))?;
    for entry in entries {
        let entry = entry.map_err(|err| io_err("read_dir entry", source, err))?;
        // 跳过隐藏临时件（`.live-core-tmp-*` 写中/崩溃孤儿）——
        // 快照只承载事实面，写中态文件没有归档价值。
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
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

/// Python `archive_current_snapshot`：归档 collection.json/streamer.json/viewers/shared；
/// ai/ 不再归档（ai/ 本就保活在 reset 之外，再复制只是磁盘churn——时间序列
/// 快照保持事实层口径，AI 侧的真凭据在图库与 input_hash 里）。
/// 没有任何候选时返回 None；同秒冲突追加 `-1/-2/...` 后缀。
pub fn archive_current_snapshot(root: &Path) -> StorageResult<Option<PathBuf>> {
    for name in LEGACY_AGGREGATE_NAMES {
        remove_file_missing_ok(&root.join(name))?;
    }
    let candidates = ["collection.json", "streamer.json", "viewers", "shared"]
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

/// Python `reset_output`：清出下轮采集的事实工作面；graph/history 长期记忆不动，
/// ai/ 认知层亦永不下刀（重采保 AI——AI 缓存的失效由 pipeline 的 per-viewer
/// input_hash 驱动：事实未变即哈希相等 → 复用零 LLM；事实变了仅该舰长重判）。
/// 原「整目录推倒」是 CLI 全量时代的混代防御，complete_cache 落地后该防御已由
/// 细粒度哈希接管，整删只剩成本副作用（重采一轮全员 AI 重跑）。
pub fn reset_output(root: &Path) -> StorageResult<()> {
    std::fs::create_dir_all(root).map_err(|err| io_err("create_dir_all", root, err))?;
    for name in ["viewers", "site", "shared"] {
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
        // tmp 唯一化后：写毕目录里只剩正主，不得残留任何 .live-core-tmp-* 孤儿。
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name != "x.json")
            .collect();
        assert!(leftovers.is_empty(), "tmp 残留：{leftovers:?}");
    }

    /// 红钉：同路径并发写不得共享固定 tmp——两个写者共用 `<file>.tmp` 时，
    /// 先 rename 的一方会把另一方的 tmp 挪走，后者 rename 必炸 NotFound（或内容撕裂）。
    /// 修复后二者各写各的唯一 tmp，双双 Ok，终态必为其中一份完整 payload。
    #[test]
    fn concurrent_writes_to_same_path_do_not_collide() {
        let root = temp_root();
        let path = root.path().join("shared").join("recap.json");
        // 删码专项复核：唯一命名使碰撞任意时序不可能——轮数/pad 不增置信度，
        // 只压 CI 时长（回归共用固定名时 barrier 对齐一轮即红）。
        let payload_a: Value = json!({"mark": "A", "pad": vec![7; 64]});
        let payload_b: Value = json!({"mark": "B", "pad": vec![9; 64]});
        for round in 0..5 {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let spawn = |payload: Value| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    write_json(&path, &payload)
                })
            };
            let a = spawn(payload_a.clone());
            let b = spawn(payload_b.clone());
            let ra = a.join().expect("thread A panicked");
            let rb = b.join().expect("thread B panicked");
            assert!(ra.is_ok(), "round {round} 写者 A 失败：{ra:?}");
            assert!(rb.is_ok(), "round {round} 写者 B 失败：{rb:?}");
            let final_value = read_json(&path)
                .unwrap()
                .unwrap_or_else(|| panic!("round {round} 终态缺文件"));
            assert!(
                final_value == payload_a || final_value == payload_b,
                "round {round} 终态被撕裂（mark={:?}）",
                final_value["mark"]
            );
        }
    }

    #[test]
    fn read_json_missing_is_none_and_bad_json_is_error() {
        let root = temp_root();
        assert_eq!(read_json(&root.path().join("nope.json")).unwrap(), None);
        let bad = root.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(read_json(&bad).unwrap_err().contains("invalid JSON"));
    }

    /// 重采保 AI：事实三面清空，graph/history（长期记忆）与 ai/（认知层缓存）
    /// 三面保活——失效归 input_hash 管，不归目录山刀管。
    #[test]
    fn reset_output_clears_fact_surface_but_keeps_memory_and_ai() {
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

        for dir in ["viewers", "site", "shared"] {
            assert!(!root.join(dir).exists(), "{dir} should be removed");
        }
        for dir in ["graph", "history", "ai"] {
            assert!(root.join(dir).join("keep.json").exists(), "{dir} preserved");
        }
        assert!(!root.join("collection.json").exists());
        assert!(!root.join("streamer.json").exists());
        assert!(!root.join("analysis.json").exists());
    }

    /// leads.jsonl 是 M4.x 线索账本——在 reset_output 白名单**外**，
    /// 必须永不被清（工作面清理不碰长期账本；「存在性钉」防改清单时灭账）。
    #[test]
    fn reset_output_never_sweeps_leads_ledger() {
        let root = temp_root();
        let root = root.path();
        std::fs::write(root.join("leads.jsonl"), "{\"dedupe_key\":\"x\"}\n").unwrap();
        reset_output(root).unwrap();
        assert!(root.join("leads.jsonl").exists(), "账本必须在 reset 中存活");
    }

    /// write/rename 之间进程崩溃会留孤儿
    /// `.{name}.live-core-tmp-*`（唯一命名不复用、不自愈）；归档若原样整拷，
    /// 垃圾态会污染时间序列快照——归档必须滤掉隐藏临时件。
    #[test]
    fn archive_skips_orphan_tmp_files() {
        let root = temp_root();
        let root = root.path();
        write_json(&root.join("viewers").join("1001.json"), &json!({"v": 1})).unwrap();
        std::fs::write(
            root.join("viewers").join(".1001.json.live-core-tmp-1-1"),
            "{\"v\":1",
        )
        .unwrap();

        let snapshot = archive_current_snapshot(root)
            .unwrap()
            .expect("archive created");
        assert!(snapshot.join("viewers").join("1001.json").exists());
        assert!(
            !snapshot
                .join("viewers")
                .join(".1001.json.live-core-tmp-1-1")
                .exists(),
            "孤儿 tmp 不得进快照"
        );
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

    /// ai/ 不在归档候选内——它在 reset 里保活，复制进快照只是磁盘 churn；
    /// 时间序列快照保持事实层口径（单有 ai/ 甚至不足以成档）。
    #[test]
    fn archive_never_sweeps_ai_dir() {
        let root = temp_root();
        let root = root.path();
        std::fs::create_dir_all(root.join("ai")).unwrap();
        write_json(
            &root.join("ai").join("state.json"),
            &json!({"status": "complete"}),
        )
        .unwrap();
        assert_eq!(
            archive_current_snapshot(root).unwrap(),
            None,
            "只有 ai/ 时不成档"
        );
        write_json(&root.join("streamer.json"), &json!({"s": 1})).unwrap();
        let snapshot = archive_current_snapshot(root)
            .unwrap()
            .expect("archive created");
        assert!(snapshot.join("streamer.json").exists());
        assert!(
            !snapshot.join("ai").exists(),
            "ai 留在原地续命，不进时间序列快照"
        );
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
