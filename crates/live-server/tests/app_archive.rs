//!「存档页」数据面钉团：存活天数 + 周健康四数 + 里程碑日历三态。
//!
//! 决定性时间注入：build_archive_payload(root, now_unix) 纯函数直呼（不依赖 HTTP
//! 时钟，数字可逐字钉死）；HTTP 层另验端点接线与响应形态。钉的固定常数
//! 2026-06-01 / 2026-08-06 与 archive.rs 单测同源（年份放 2026，避开日历炸弹）。

use std::path::Path;

use serde_json::{Value, json};
use tower::ServiceExt;

mod common;

use axum::http::Request;
use http_body_util::BodyExt;

use live_server::app::{AppState, build_app, build_archive_payload};
use live_server::registry::Registry;

/// 2026-08-06T00:00:00Z 的固定 unix 秒。
const AUG6_2026: i64 = 1_785_974_400;

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string(value).unwrap()).unwrap();
}

fn collection(viewer_count: i64, started_at: &str) -> Value {
    json!({
        "status": "complete",
        "viewer_count": viewer_count,
        "guard_count": viewer_count,
        "started_at": started_at,
        "finished_at": "2026-08-01T02:00:00.000000+00:00",
    })
}

fn streamer(followers: i64) -> Value {
    json!({ "profile": { "name": "测试主播", "followers": followers } })
}

fn recap(speakers: i64, repeated_count: i64, text: &str) -> Value {
    json!({
        "status": "ready",
        "speakers": speakers,
        "headline": "h",
        "session": {"start": "2026-08-01T00:00:00Z", "end": "2026-08-01T01:00:00Z", "rid": "1"},
        "repeated": {"text": text, "count": repeated_count},
    })
}

/// follower_snapshots.jsonl 逐行追加写（追加语义 = 真实落盘形态）。
fn append_follower_snapshot(root: &Path, ts: &str, followers: i64) {
    let path = root.join("history").join("follower_snapshots.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    use std::io::Write as _;
    writeln!(f, "{}", json!({"ts": ts, "followers": followers})).unwrap();
}

fn health_row<'a>(body: &'a Value, key: &str) -> &'a Value {
    body["weekly_health"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
        .unwrap()
}

fn milestone<'a>(body: &'a Value, key: &str) -> &'a Value {
    body["milestones"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
        .unwrap()
}

/// 全就位钉：四数实数 + 五项里程碑三态齐活（锚点 = 双证最早 2026-06-01，66 天）。
#[test]
fn full_fixture_known_rows_and_milestone_states() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_json(
        &root.join("collection.json"),
        &collection(25, "2026-08-01T00:00:00.000000+00:00"),
    );
    write_json(&root.join("streamer.json"), &streamer(1300));
    write_json(
        &root.join("ai").join("recap.json"),
        &recap(4, 3, "晚上好！"),
    );
    // 两档快照：旧档 06-01（18 舰）与新档 07-01（22 舰 = 上一轮值）。目录名
    // 时间戳与夹内 collection.started_at 双证：取锚一致落在 06-01。
    for (stamp, viewer_count) in [("20260601T000000Z", 18), ("20260701T000000Z", 22)] {
        write_json(
            &root
                .join("history")
                .join("snapshots")
                .join(stamp)
                .join("collection.json"),
            &collection(
                viewer_count,
                &format!("{}{}", &stamp[..8], "T00:00:00.000000+00:00"),
            ),
        );
    }
    append_follower_snapshot(root, "2026-07-01T00:00:00Z", 1000);
    append_follower_snapshot(root, "2026-08-01T00:00:00Z", 1002);

    let body = build_archive_payload(root, AUG6_2026);

    // 存活：双证最早锚 = 2026-06-01 → 66 天（6/1 → 8/6）。
    assert_eq!(body["alive_days"], 66);
    assert_eq!(body["alive_since"], "2026-06-01T00:00:00.000000+00:00");

    // 周健康四数。
    assert_eq!(health_row(&body, "repeat_rate")["known"], true);
    assert_eq!(
        health_row(&body, "repeat_rate")["value_text"],
        "75%（3 次 / 4 人）"
    );
    assert_eq!(health_row(&body, "core_danmaku_group")["known"], false);
    assert_eq!(
        health_row(&body, "core_danmaku_group")["value_text"],
        "名册分档未就位（D2 冻结）"
    );
    assert_eq!(health_row(&body, "guard_delta")["known"], true);
    assert_eq!(
        health_row(&body, "guard_delta")["value_text"],
        "+3（上轮 22 → 现轮 25）"
    );
    assert_eq!(health_row(&body, "follower_delta")["known"], true);
    assert_eq!(
        health_row(&body, "follower_delta")["value_text"],
        "+2（1,000 → 1,002）"
    );

    // 里程碑日历五件三态。
    assert_eq!(milestone(&body, "full_moon")["state"], "done");
    assert_eq!(
        milestone(&body, "full_moon")["detail_text"],
        "已于 2026-07-01 达成（存活 66 天）"
    );
    assert_eq!(milestone(&body, "hundred_days")["state"], "pending");
    assert_eq!(
        milestone(&body, "hundred_days")["detail_text"],
        "还差 34 天（存活 66 天）"
    );
    assert_eq!(milestone(&body, "thousand_followers")["state"], "done");
    assert_eq!(
        milestone(&body, "thousand_followers")["detail_text"],
        "粉丝 1300（目标 1000）达成"
    );
    assert_eq!(milestone(&body, "hundred_guards")["state"], "pending");
    assert_eq!(
        milestone(&body, "hundred_guards")["detail_text"],
        "还差 75 舰（当前 25）"
    );
    assert_eq!(milestone(&body, "anniversary")["state"], "pending");
    assert_eq!(
        milestone(&body, "anniversary")["detail_text"],
        "还差 299 天（存活 66 天）"
    );
    // 里程碑键序钉在 empty_fixture_all_unknown_rows（唯一处，防双面同句漂移）。
}

/// 缺件组合：无快照（大航海 delta 未知），其余 pending——钉「缺哪条哪条 unknown」。
#[test]
fn pending_fixture_without_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_json(
        &root.join("collection.json"),
        &collection(50, "2026-08-01T00:00:00.000000+00:00"),
    );
    write_json(&root.join("streamer.json"), &streamer(500));
    write_json(
        &root.join("ai").join("recap.json"),
        &recap(4, 4, "晚上好！"),
    );

    let body = build_archive_payload(root, AUG6_2026);

    assert_eq!(body["alive_days"], 5); // 锚 = 当前 collection.start 08-01。
    assert_eq!(
        health_row(&body, "repeat_rate")["value_text"],
        "100%（4 次 / 4 人）"
    );
    assert_eq!(health_row(&body, "guard_delta")["known"], false);
    assert_eq!(
        health_row(&body, "guard_delta")["value_text"],
        "上轮舰长数据未就位"
    );
    assert_eq!(health_row(&body, "follower_delta")["known"], false);
    assert_eq!(
        health_row(&body, "follower_delta")["value_text"],
        "快照未就位（刚建账）"
    );

    assert_eq!(milestone(&body, "full_moon")["state"], "pending");
    assert_eq!(
        milestone(&body, "full_moon")["detail_text"],
        "还差 25 天（存活 5 天）"
    );
    assert_eq!(
        milestone(&body, "hundred_days")["detail_text"],
        "还差 95 天（存活 5 天）"
    );
    assert_eq!(
        milestone(&body, "thousand_followers")["detail_text"],
        "还差 500 粉（当前 500）"
    );
    assert_eq!(
        milestone(&body, "hundred_guards")["detail_text"],
        "还差 50 舰（当前 50）"
    );
    assert_eq!(
        milestone(&body, "anniversary")["detail_text"],
        "还差 360 天（存活 5 天）"
    );
}

/// 空根：全 unknown —— 无锚点 / 无四数 / 五项全「未就位」。
#[test]
fn empty_fixture_all_unknown_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let body = build_archive_payload(tmp.path(), AUG6_2026);

    assert_eq!(body["alive_days"], Value::Null);
    assert_eq!(body["alive_since"], Value::Null);
    for (key, text) in [
        ("repeat_rate", "复读率未就位"),
        ("core_danmaku_group", "名册分档未就位（D2 冻结）"),
        ("guard_delta", "上轮舰长数据未就位"),
        ("follower_delta", "快照未就位（刚建账）"),
    ] {
        assert_eq!(health_row(&body, key)["known"], false, "key={key}");
        assert_eq!(health_row(&body, key)["value_text"], text, "key={key}");
    }
    for (key, text) in [
        ("full_moon", "起始锚点未就位"),
        ("hundred_days", "起始锚点未就位"),
        ("thousand_followers", "粉丝数未就位"),
        ("hundred_guards", "舰长数未就位"),
        ("anniversary", "起始锚点未就位"),
    ] {
        assert_eq!(milestone(&body, key)["state"], "unknown", "key={key}");
        assert_eq!(milestone(&body, key)["detail_text"], text, "key={key}");
    }
    // 顺序钉：周健康 [复读率, 核心弹幕团, 大航海 delta, 涨粉 delta]。
    let keys: Vec<&str> = body["weekly_health"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "repeat_rate",
            "core_danmaku_group",
            "guard_delta",
            "follower_delta"
        ]
    );
    // 键序 = 规格文序（前端按数组序直渲，只钉字面序）。
    let keys: Vec<&str> = body["milestones"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "full_moon",
            "hundred_days",
            "anniversary",
            "thousand_followers",
            "hundred_guards",
        ]
    );
}

/// 涨粉 delta 边界：坏行跳过 + 负 delta；全同值 → 0 且注明「自建账起未变」。
#[test]
fn follower_delta_negative_and_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    append_follower_snapshot(root, "2026-07-01T00:00:00Z", 1002);
    // 坏行（缺 followers / 非 JSON / 空行）必须被整行跳过，不影响末行口径。
    let snapshots_path = root.join("history").join("follower_snapshots.jsonl");
    std::fs::create_dir_all(snapshots_path.parent().unwrap()).unwrap();
    std::fs::write(
        &snapshots_path,
        "{\"ts\":\"2026-07-01T00:00:00Z\",\"followers\":1002}\n\
         这不是 JSON\n\
         {\"ts\":\"x\"}\n\
         \n\
         {\"ts\":\"2026-08-01T00:00:00Z\",\"followers\":999}\n",
    )
    .unwrap();
    let body = build_archive_payload(root, AUG6_2026);
    assert_eq!(health_row(&body, "follower_delta")["known"], true);
    assert_eq!(
        health_row(&body, "follower_delta")["value_text"],
        "-3（1,002 → 999）"
    );

    // 全同值 → delta 0 + 注明。
    let tmp2 = tempfile::tempdir().unwrap();
    let root2 = tmp2.path();
    let snapshots_path2 = root2.join("history").join("follower_snapshots.jsonl");
    std::fs::create_dir_all(snapshots_path2.parent().unwrap()).unwrap();
    std::fs::write(
        &snapshots_path2,
        "{\"ts\":\"2026-07-01T00:00:00Z\",\"followers\":1002}\n\
         {\"ts\":\"2026-08-01T00:00:00Z\",\"followers\":1002}\n",
    )
    .unwrap();
    let body2 = build_archive_payload(root2, AUG6_2026);
    assert_eq!(
        health_row(&body2, "follower_delta")["value_text"],
        "0（1,002，自建账起未变）"
    );
}

/// 锚点来源四：图库场次最早 observed_at 单独作锚（无归档快照/collection 时）。
#[test]
fn graph_episode_earliest_serves_as_anchor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let store_path = root.join("graph").join("perception.sqlite3");
    std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
    let store = live_core::graph::store::Store::open(&store_path).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&store_path).unwrap();
    conn.execute(
        "INSERT INTO episodes \
         (episode_id, viewer_id, source, event_type, observed_at, published_at, title, \
          url, bvid, fields_json, platform_facts_json, content_hash, first_seen_at, last_seen_at) \
         VALUES ('ep:anchor', '', 'ai_state', 'chat', \
                 '2026-05-01T00:00:00.000000+00:00', NULL, NULL, NULL, NULL, \
                 '[]', '{}', 'h', '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);

    let body = build_archive_payload(root, AUG6_2026);
    // 2026-05-01 → 2026-08-06 = 97 天（5/1→6/1 31 天 + 6/1→7/1 30 天 + 7/1→8/1 31 天 + 5 天）。
    assert_eq!(body["alive_days"], 97);
    assert_eq!(body["alive_since"], "2026-05-01T00:00:00.000000+00:00");
    // 图库无归档快照 → 大航海 delta 仍 unknown（与锚点来源无关的独立行）。
    assert_eq!(health_row(&body, "guard_delta")["known"], false);
}

/// HTTP 接线钉：GET /api/archive → 200 且响应形完整（真实时钟，只钉形态不断言日数）。
#[tokio::test(flavor = "multi_thread")]
async fn endpoint_returns_archive_payload_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    std::fs::create_dir_all(&output_dir).unwrap();
    write_json(
        &output_dir.join("collection.json"),
        &collection(25, "2026-08-01T00:00:00.000000+00:00"),
    );
    write_json(&output_dir.join("streamer.json"), &streamer(1300));
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        common::yaml_template(
            None,
            "m5b-archive",
            "SESSDATA=test",
            "test",
            "http://127.0.0.1:9/v1",
            "m5b-archive",
        )
        .replace(
            "OUTPUT_DIR",
            &output_dir.display().to_string().replace('\\', "/"),
        ),
    )
    .unwrap();
    let app = build_app(AppState {
        config_path: config_path.clone(),
        web_root: tmp.path().join("no-dist"),
        registry: Registry::new(),
        demo: false,
        data_root: Some(output_dir.clone()),
        bilibili_hosts: None,
        config_write_lock: Default::default(),
        graph_artifact_lock: Default::default(),
    });

    let request = Request::builder()
        .uri("/api/archive")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    assert!(body.get("alive_days").unwrap().is_i64(), "{body}");
    assert!(body["alive_since"].is_string(), "{body}");
    let health = body["weekly_health"].as_array().unwrap();
    assert_eq!(health.len(), 4);
    for row in health {
        for key in ["key", "label", "value_text"] {
            assert!(row[key].is_string(), "{row}");
        }
        assert!(row["known"].is_boolean(), "{row}");
    }
    let milestones = body["milestones"].as_array().unwrap();
    assert_eq!(milestones.len(), 5);
    for row in milestones {
        for key in ["key", "label", "state", "detail_text"] {
            assert!(row[key].is_string(), "{row}");
        }
        assert!(
            ["done", "pending", "unknown"].contains(&row["state"].as_str().unwrap()),
            "{row}"
        );
    }
}
