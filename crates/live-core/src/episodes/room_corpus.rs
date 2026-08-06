//! 房间语料 Episode 化（迭代细则 v1 §1 P0-1）：`shared/replay_danmaku.json` 与
//! `shared/room_comments.json` 的行 → 不可变 Episode，走既有 ingest 通道落图。
//!
//! 体积备书（轮3）：超 500 线 = 两语料入口 × 行→Episode 投影，共享每行 parity
//! 选择（shard_index 打标/行序稳定化）的单通道语义；一文件一端口不拆。
//!
//! 归属裁决（细则原文）：
//! - viewer_id = `"_room"`（保留命名空间，与 `_demo` 同款隔离；**不做观众级归属**——
//!   弹幕 uid/评论 mid 只进 platform_facts，实体发现留给 mention 层）。
//! - Episode 身份：comment = `rpid`；danmaku = `(rid, shard_index, 行序)`。
//!   行序靠端点保序拼接（`getInfoByLiveRecord.dm_info.num` 分片顺序）+ 每行
//!   `shard_index` 打标（endpoints.rs 注入，零新增请求）保证跨 run 稳定。
//! - fields = `[{path: "text", kind: "text"}]`（全文，mention 可定位）；
//!   空文本行保留 Episode（密度/人头是事实），但零 fields（mention 不可定位）。
//! - platform_facts 键集照抄细则：`{sender_uid_crc|mid, like, ctime|ts, rid/rpid,
//!   session:{start,end}}`——投诉 `creator_name/tags/platform_category` 三键即会
//!   触发现货实体边（graph/build.rs），本模块**刻意不产出**这三键（uid 脱敏边界）。
//! - 场次窗：danmaku 取回放束 start/end_timestamp；comment 用 ctime。
//!
//! content_version 公式与 build.rs 逐字节一致（同一 version_doc 键序 + json_canon +
//! sha1-16）——两条 Episode 生产线同指纹纪律，撞库复用 upsert_episode_inner。

use std::path::Path;

use serde_json::{Map, Value};

use crate::graph;
use crate::graph::Store;
use crate::graph::store::Result;

use super::{Episode, EpisodeField, hash_parts, json_canon, now_iso, py_str};

/// 保留命名空间：房间语料不做观众级归属，全部挂在 `_room` 名下（细则 §1 P0-1）。
pub const ROOM_VIEWER_ID: &str = "_room";
/// 弹幕 Episode 的 source（与字段 payload 文件名对齐）。
pub const SOURCE_LIVE_DANMAKU: &str = "live_danmaku";
/// 弹幕 Episode 的 event_type。
pub const EVENT_LIVE_DANMAKU: &str = "live_danmaku";
/// 评论 Episode 的 source（沿用采集端 `normalize_comment` 的 source 值）。
pub const SOURCE_COMMENT: &str = "comment";
/// 评论 Episode 的 event_type。
pub const EVENT_ROOM_COMMENT: &str = "room_comment";

/// unix 秒 → ISO 8661 字符串（与 now_iso 同款六位微秒 + +00:00 尾）。
/// 不可解析（0/负）一律空串——published_at 空值是非致命形态（build.rs 同款）。
fn unix_to_iso(raw: &Value) -> String {
    // B12：解析+格式化沉入 episodes 公共区（value_unix_secs+unix_secs_to_iso）。
    super::unix_secs_to_iso(super::value_unix_secs(raw))
}

/// version_doc → content_version（**与 build.rs 的公式逐字节一致**，键序固定）。
/// 8 参数全是构建件（身份三元组 + 双时间戳 + fields/facts），私有两调用点，
/// 压成 struct 只是为 lint 硬凑层（AGENTS.md §4）。
/// D1 WS 弹幕窗（2B）经 `crate::episodes::room_corpus::finalize_episode` 复用本公式——
/// 两条 Episode 生产线同指纹纪律（撞库幂等语义天然共享）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_episode(
    viewer_id: &str,
    stable: &str,
    source: &str,
    event_type: &str,
    published_at: &str,
    fields: Vec<EpisodeField>,
    platform_facts: Value,
    observed_at: &str,
) -> Episode {
    let version_doc = serde_json::json!({
        "source": source,
        "event_type": event_type,
        "published_at": published_at,
        "title": "",
        "url": "",
        "bvid": "",
        "fields": fields.iter().map(|f| f.to_json()).collect::<Vec<_>>(),
        "platform_facts": platform_facts.clone(),
    });
    let content_version = hash_parts(&[json_canon(&version_doc)], 16);
    Episode {
        episode_id: format!("episode:{viewer_id}:{stable}:{content_version}"),
        viewer_id: viewer_id.to_string(),
        source: source.to_string(),
        event_type: event_type.to_string(),
        observed_at: observed_at.to_string(),
        published_at: published_at.to_string(),
        title: String::new(),
        url: String::new(),
        bvid: String::new(),
        fields,
        platform_facts,
    }
}

/// 单条弹幕行 → Episode。`shard`/`line_index` 由调用方按端点打标与枚举序传入。
/// 空 text 返回带零 fields 的 Episode（保留密度事实，mention 层不可定位）。
pub fn danmaku_to_episode(
    rid: &str,
    session_start: &Value,
    session_end: &Value,
    shard: i64,
    line_index: i64,
    row: &Value,
    observed_at: &str,
) -> Episode {
    let text = py_str(row.get("text").unwrap_or(&Value::Null));
    let fields = if text.is_empty() {
        Vec::new()
    } else {
        vec![EpisodeField {
            path: "text".to_string(),
            text,
            kind: "text".to_string(),
        }]
    };
    let mut facts = Map::new();
    facts.insert(
        "sender_uid_crc".to_string(),
        Value::String(py_str(row.get("uid").unwrap_or(&Value::Null))),
    );
    let ts = py_str(row.get("ts").unwrap_or(&Value::Null));
    if !ts.is_empty() {
        facts.insert("ts".to_string(), Value::String(ts));
    }
    facts.insert("rid".to_string(), Value::String(rid.to_string()));
    facts.insert("shard_index".to_string(), serde_json::json!(shard));
    facts.insert("line_index".to_string(), serde_json::json!(line_index));
    facts.insert(
        "session".to_string(),
        serde_json::json!({
            "start_timestamp": session_start,
            "end_timestamp": session_end,
        }),
    );
    finalize_episode(
        ROOM_VIEWER_ID,
        &format!("danmaku:{rid}:{shard}:{line_index}"),
        SOURCE_LIVE_DANMAKU,
        EVENT_LIVE_DANMAKU,
        &unix_to_iso(session_start),
        fields,
        Value::Object(facts),
        observed_at,
    )
}

/// 单条评论行（`normalize_comment` 产出形态）→ Episode。身份 = rpid。
pub fn comment_to_episode(row: &Value, observed_at: &str) -> Option<Episode> {
    let rpid = py_str(row.get("rpid").unwrap_or(&Value::Null));
    if rpid.is_empty() {
        // 无 rpid 无身份——幂等钉无从谈起，跳过并留痕（返回 None，由调用方计数）。
        return None;
    }
    let text = py_str(row.get("message").unwrap_or(&Value::Null));
    let fields = if text.is_empty() {
        Vec::new()
    } else {
        vec![EpisodeField {
            path: "text".to_string(),
            text,
            kind: "text".to_string(),
        }]
    };
    let ctime = row.get("ctime").cloned().unwrap_or(Value::Null);
    let mut facts = Map::new();
    facts.insert("rpid".to_string(), Value::String(rpid.clone()));
    facts.insert(
        "mid".to_string(),
        Value::String(py_str(row.get("mid").unwrap_or(&Value::Null))),
    );
    facts.insert("ctime".to_string(), ctime.clone());
    facts.insert(
        "target_kind".to_string(),
        Value::String(py_str(row.get("target_kind").unwrap_or(&Value::Null))),
    );
    facts.insert(
        "target_oid".to_string(),
        Value::String(py_str(row.get("target_oid").unwrap_or(&Value::Null))),
    );
    let like = py_str(row.get("like").unwrap_or(&Value::Null));
    if !like.is_empty() {
        facts.insert("like".to_string(), Value::String(like));
    }
    Some(finalize_episode(
        ROOM_VIEWER_ID,
        &format!("comment:{rpid}"),
        SOURCE_COMMENT,
        EVENT_ROOM_COMMENT,
        &unix_to_iso(&ctime),
        fields,
        Value::Object(facts),
        observed_at,
    ))
}

/// replay_danmaku.json payload → 全部弹幕 Episode（跨全部回放束）。
pub fn replay_danmaku_episodes(payload: &Value, observed_at: &str) -> Vec<Episode> {
    let mut out = Vec::new();
    let records = match payload.get("records").and_then(Value::as_array) {
        Some(records) => records,
        None => return out,
    };
    for record in records {
        let rid = py_str(record.get("rid").unwrap_or(&Value::Null));
        if rid.is_empty() {
            continue;
        }
        let start = record
            .get("start_timestamp")
            .cloned()
            .unwrap_or(Value::Null);
        let end = record.get("end_timestamp").cloned().unwrap_or(Value::Null);
        let messages = match record.get("messages").and_then(Value::as_array) {
            Some(messages) => messages,
            None => continue,
        };
        for (line_index, row) in messages.iter().enumerate() {
            // shard_index 由端点上行打标（endpoints.rs）；缺标行跳过——身份
            // 三元组少一格即不稳定，宁缺毋错（计数差异由 source_status 暴露）。
            let shard = match row.get("shard_index").and_then(Value::as_i64) {
                Some(shard) => shard,
                None => continue,
            };
            out.push(danmaku_to_episode(
                &rid,
                &start,
                &end,
                shard,
                line_index as i64,
                row,
                observed_at,
            ));
        }
    }
    out
}

/// room_comments.json payload → 全部评论 Episode。
pub fn room_comment_episodes(payload: &Value, observed_at: &str) -> Vec<Episode> {
    payload
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| comment_to_episode(row, observed_at))
                .collect()
        })
        .unwrap_or_default()
}

/// 读 shared 目录两份语料 → Episode 列表。文件缺失/解析失败按空集处理并
/// 返回 (episodes, 每源行数)——记账颗粒度留给调用方（source_status 独立键）。
pub fn room_corpus_episodes(shared_dir: &Path) -> (Vec<Episode>, Value) {
    let observed_at = now_iso();
    let mut out = Vec::new();
    let mut counts = Map::new();

    let danmaku_path = shared_dir.join("replay_danmaku.json");
    let danmaku = std::fs::read_to_string(&danmaku_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|payload| replay_danmaku_episodes(&payload, &observed_at))
        .unwrap_or_default();
    counts.insert("live_danmaku".to_string(), serde_json::json!(danmaku.len()));
    out.extend(danmaku);

    let comments_path = shared_dir.join("room_comments.json");
    let comments = std::fs::read_to_string(&comments_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|payload| room_comment_episodes(&payload, &observed_at))
        .unwrap_or_default();
    counts.insert(
        "room_comment".to_string(),
        serde_json::json!(comments.len()),
    );
    out.extend(comments);

    (out, Value::Object(counts))
}

/// 房间语料入账：先立 `viewer:_room` 守卫节点（FK edges→nodes 的宿主，形态
/// 同 apply_guard_edges 的 Viewer 节点），再逐条走既有 ingest 通道。
/// 幂等语义 = upsert_episode_inner 撞库：hash 同只刷 last_seen_at，不同报错。
pub fn ingest_room_corpus(store: &Store, run_id: &str, episodes: &[Episode]) -> Result<()> {
    store.upsert_node(
        &format!("viewer:{ROOM_VIEWER_ID}"),
        "Viewer",
        ROOM_VIEWER_ID,
        &serde_json::json!({"viewer_id": ROOM_VIEWER_ID, "reserved_namespace": true}),
        "platform_fact",
        None,
    )?;
    for episode in episodes {
        graph::build::ingest_platform_facts(store, run_id, episode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::episodes::seeds;

    fn fixture_row(shard: i64) -> Value {
        json!({"text": "好耶", "uid": "u-42", "shard_index": shard, "ts": "1735723200"})
    }

    #[test]
    fn danmaku_episode_id_stable_and_sensitive_to_line_order() {
        let row = fixture_row(0);
        let a = danmaku_to_episode(
            "rid-1",
            &json!(1735723200),
            &json!(1735726800),
            0,
            7,
            &row,
            "obs",
        );
        let b = danmaku_to_episode(
            "rid-1",
            &json!(1735723200),
            &json!(1735726800),
            0,
            7,
            &row,
            "other-obs",
        );
        assert_eq!(a.episode_id, b.episode_id, "observed_at 不得进入身份");
        let c = danmaku_to_episode(
            "rid-1",
            &json!(1735723200),
            &json!(1735726800),
            0,
            8,
            &row,
            "obs",
        );
        assert_ne!(
            a.episode_id, c.episode_id,
            "行序变即身份变（幂等公式 (rid,shard,行序)）"
        );
        assert!(a.episode_id.starts_with("episode:_room:danmaku:rid-1:0:7:"));
    }

    #[test]
    fn danmaku_episode_field_text_locates_mention_seed() {
        let a = danmaku_to_episode(
            "rid-1",
            &json!(1735723200),
            &json!(1735726800),
            0,
            0,
            &fixture_row(0),
            "obs",
        );
        assert_eq!(a.viewer_id, ROOM_VIEWER_ID);
        assert_eq!(a.source, SOURCE_LIVE_DANMAKU);
        assert_eq!(a.event_type, EVENT_LIVE_DANMAKU);
        // 验收钉②：弹幕行 → validate_span 可定位的 mention seed（整段 + 子串都过）
        assert_eq!(seeds::validate_span(&a, "text", "好耶", 0, 2), None);
        assert_eq!(seeds::validate_span(&a, "text", "耶", 1, 2), None);
        assert!(seeds::validate_span(&a, "text", "错", 0, 1).is_some());
        assert!(a.fields.len() == 1 && a.fields[0].kind == "text");
        // 脱敏边界：uid 进 facts，且 facts 不产现货三键（否则触发实体边）
        assert_eq!(a.platform_facts["sender_uid_crc"], json!("u-42"));
        for banned in ["creator_name", "tags", "platform_category"] {
            assert!(
                a.platform_facts.get(banned).is_none(),
                "uid 边界：不得产出 {banned}"
            );
        }
        assert_eq!(
            a.platform_facts["session"]["start_timestamp"],
            json!(1735723200)
        );
        assert_eq!(
            a.published_at,
            unix_to_iso(&json!(1735723200)),
            "场次窗：published_at = 回放束 start"
        );
    }

    #[test]
    fn empty_text_danmaku_kept_with_zero_fields() {
        let row = json!({"text": "", "uid": "u", "shard_index": 1});
        let ep = danmaku_to_episode("rid-1", &json!(1), &json!(2), 1, 0, &row, "obs");
        assert!(ep.fields.is_empty(), "空文本行保留 Episode 但零 fields");
        assert_eq!(ep.platform_facts["line_index"], json!(0));
    }

    #[test]
    fn comment_episode_id_uses_rpid() {
        let row = json!({"rpid": "9988", "mid": "m-7", "message": "早", "ctime": "1735723200", "target_kind": "video", "target_oid": "BV1"});
        let a = comment_to_episode(&row, "obs").expect("comment");
        let b = comment_to_episode(&row, "obs").expect("comment");
        assert_eq!(a.episode_id, b.episode_id);
        assert!(a.episode_id.starts_with("episode:_room:comment:9988:"));
        let c = comment_to_episode(&json!({"rpid": "9989", "message": "早"}), "obs").unwrap();
        assert_ne!(a.episode_id, c.episode_id);
        assert_eq!(a.source, SOURCE_COMMENT);
        assert_eq!(a.event_type, EVENT_ROOM_COMMENT);
        assert_eq!(seeds::validate_span(&a, "text", "早", 0, 1), None);
        assert_eq!(a.platform_facts["mid"], json!("m-7"));
        // 无 rpid = 无身份 → 跳过
        assert!(comment_to_episode(&json!({"message": "x"}), "obs").is_none());
    }

    #[test]
    fn bundle_converts_and_skips_untagged_rows() {
        let payload = json!({"records": [{
            "rid": "R1", "title": "t",
            "start_timestamp": 1735723200, "end_timestamp": 1735726800,
            "messages": [
                {"text": "A", "uid": "1", "shard_index": 0},
                {"text": "B", "uid": "2"}, // 缺 shard_index → 跳过
                {"text": "C", "uid": "3", "shard_index": 1},
            ],
        }]});
        let episodes = replay_danmaku_episodes(&payload, "obs");
        assert_eq!(episodes.len(), 2, "缺 shard 打标行须跳过: {episodes:?}");
        assert_eq!(episodes[0].platform_facts["line_index"], json!(0));
        assert_eq!(episodes[1].platform_facts["line_index"], json!(2));
    }

    #[test]
    fn room_corpus_reads_shared_dir_and_reports_per_source_counts() {
        let dir = tempdir().unwrap();
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(
            shared.join("replay_danmaku.json"),
            json!({"records": [{"rid": "R", "start_timestamp": 1, "end_timestamp": 2,
                "messages": [{"text": "A", "uid": "1", "shard_index": 0}]}]})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            shared.join("room_comments.json"),
            json!({"rows": [{"rpid": "1", "message": "好", "ctime": "1735723200"}]}).to_string(),
        )
        .unwrap();
        let (episodes, counts) = room_corpus_episodes(&shared);
        assert_eq!(episodes.len(), 2);
        assert_eq!(counts, json!({"live_danmaku": 1, "room_comment": 1}));
        // 缺文件 → 空集不炸
        let (episodes, counts) = room_corpus_episodes(&dir.path().join("nowhere"));
        assert!(episodes.is_empty());
        assert_eq!(counts, json!({"live_danmaku": 0, "room_comment": 0}));
    }

    #[test]
    fn ingest_twice_does_not_grow_episode_rows_and_never_pays_viewer_files() {
        let dir = tempdir().unwrap();
        let store = Store::open(&dir.path().join("graph.sqlite3")).expect("store opens");
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(
            shared.join("room_comments.json"),
            json!({"rows": [
                {"rpid": "1", "message": "早上好", "ctime": "1735723200"},
                {"rpid": "2", "message": "晚上好", "ctime": "1735726800"},
            ]})
            .to_string(),
        )
        .unwrap();
        let (episodes, _counts) = room_corpus_episodes(&shared);
        assert_eq!(episodes.len(), 2);
        // 验收钉①：重跑行数不增（幂等）。run 行必须存在——edges.run_id 是
        // graph_runs 真外键（schema v6）。
        for run in ["run-a", "run-b", "run-c"] {
            store
                .begin_run_fixed(run, &now_iso(), "test")
                .expect("begin run");
        }
        ingest_room_corpus(&store, "run-a", &episodes).expect("ingest first");
        let first = crate::graph::query::episodes(&store, ROOM_VIEWER_ID, None)
            .expect("query")
            .len();
        ingest_room_corpus(&store, "run-b", &episodes).expect("ingest second");
        let second = crate::graph::query::episodes(&store, ROOM_VIEWER_ID, None)
            .expect("query")
            .len();
        assert_eq!(first, 2);
        assert_eq!(first, second, "重跑 episodes 行数不得增长");
        // 边界钉③：观众 files 零污染——_room 只活在图里（viewer 节点），
        // 观众目录没有任何 "_room.json" 类文件落成（本模块只读 shared 不涉水）。
        let viewers_dir = dir.path().join("viewers");
        if viewers_dir.exists() {
            let stray: Vec<_> = std::fs::read_dir(&viewers_dir)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().contains(ROOM_VIEWER_ID))
                .collect();
            assert!(stray.is_empty(), "viewers/ 不得出现 _room 文件: {stray:?}");
        }
        // content 变化必须撞库报错（复用 upsert 既有纪律）
        let mut tampered = episodes[0].clone();
        tampered.fields[0].text = "被改写".to_string();
        // episode_id 带旧 content_version 但 fields 被改 → hash 不同 → Err
        let err = ingest_room_corpus(&store, "run-c", &[tampered]).unwrap_err();
        assert!(
            format!("{err}").contains("immutable Episode conflict"),
            "撞库拒绝须报错而非静默覆盖: {err}"
        );
    }
}
