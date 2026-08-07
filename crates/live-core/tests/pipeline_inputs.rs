//! M4-A golden 对账：pipeline 输入小件 5 件套 与 Python 预言机真值 fixtures 比对。
//! fixtures = tests-fixtures/m4a/（由私仓 target/gen_m4a_golden.py 调 Python 实算产出）。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use live_core::agent::pipeline::{
    aggregate_runtime_usage, audience_input_hash_material, build_audience_input, canonical_json,
    compact_interest_state, episode_set_hash, stable_hash, viewer_input_bundle,
};
use live_core::agent::runtime::OaiUsage;
use live_core::episodes::baseline::{build_factual_baseline, viewer_context};

fn m4a() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-fixtures/m4a")
}

fn load(name: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(m4a().join(name)).expect("fixture readable"))
        .expect("fixture is JSON")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// stable_hash / canonical_json（Python ai_data.stable_hash）
// ---------------------------------------------------------------------------

#[test]
fn stable_hash_matches_python_golden() {
    for case in load("stable_hash_cases.json").as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert_eq!(
            canonical_json(&case["input"]),
            case["canonical"].as_str().unwrap(),
            "canonical mismatch: {name}"
        );
        assert_eq!(
            stable_hash(&case["input"]),
            case["hash"].as_str().unwrap(),
            "hash mismatch: {name}"
        );
    }
}

// ---------------------------------------------------------------------------
// build_factual_baseline（Python episodes.build_factual_baseline）
// ---------------------------------------------------------------------------

const MAX_EVIDENCE: usize = 1000;

#[test]
fn baseline_matches_python_golden() {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&m4a().join("viewer_root"), tmp.path());
    let baseline = build_factual_baseline(tmp.path(), MAX_EVIDENCE).expect("baseline builds");
    assert_eq!(baseline, load("baseline_expected.json"));
}

#[test]
fn baseline_missing_collection_error_parity() {
    let tmp = tempfile::tempdir().unwrap();
    let err = build_factual_baseline(tmp.path(), MAX_EVIDENCE).unwrap_err();
    assert_eq!(
        err.to_string(),
        "collection is not complete (status=missing)"
    );
}

#[test]
fn baseline_running_status_error_parity() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("collection.json"),
        json!({"status": "running"}).to_string(),
    )
    .unwrap();
    let err = build_factual_baseline(tmp.path(), MAX_EVIDENCE).unwrap_err();
    assert_eq!(
        err.to_string(),
        "collection is not complete (status=running)"
    );
}

#[test]
fn baseline_no_viewers_error_parity() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("viewers")).unwrap();
    fs::write(
        tmp.path().join("collection.json"),
        json!({"status": "complete"}).to_string(),
    )
    .unwrap();
    let err = build_factual_baseline(tmp.path(), MAX_EVIDENCE).unwrap_err();
    assert_eq!(err.to_string(), "no viewer files found; run collect first");
}

// ---------------------------------------------------------------------------
// viewer_context（Python ai_data.viewer_context：catalog 缺失 → 现算 evidence）
// ---------------------------------------------------------------------------

#[test]
fn viewer_context_recomputes_when_catalog_missing() {
    let baseline = load("baseline_expected.json");
    let profile = baseline["viewer_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["viewer"]["id"] == "g1")
        .unwrap()
        .clone();
    let mut stripped = profile.as_object().unwrap().clone();
    stripped.remove("evidence_catalog");
    let viewer: Value = serde_json::from_str(
        &fs::read_to_string(m4a().join("viewer_root/viewers/g1.json")).unwrap(),
    )
    .unwrap();
    let context = viewer_context(&viewer, &Value::Object(stripped), MAX_EVIDENCE);
    assert_eq!(context, load("viewer_context_expected.json"));
}

// ---------------------------------------------------------------------------
// viewer_input_bundle（Python pipeline._viewer_input_bundle：payload + input_hash）
// ---------------------------------------------------------------------------

#[test]
fn viewer_input_bundle_matches_python() {
    let expected = load("bundle_expected.json");
    let baseline = load("baseline_expected.json");
    let profile = baseline["viewer_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["viewer"]["id"] == "g1")
        .unwrap()
        .clone();
    let viewer: Value = serde_json::from_str(
        &fs::read_to_string(m4a().join("viewer_root/viewers/g1.json")).unwrap(),
    )
    .unwrap();
    let settings = &expected["settings"];
    let rules: Vec<String> = settings["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect();
    let bundle = viewer_input_bundle(
        &viewer,
        &profile,
        settings["model"].as_str().unwrap(),
        settings["api"].as_str().unwrap(),
        &settings["reasoning"],
        &rules,
        MAX_EVIDENCE,
    );
    assert_eq!(
        bundle.episodes.len(),
        expected["episode_count"].as_u64().unwrap() as usize
    );
    assert_eq!(bundle.input_payload, expected["input_payload"]);
    // hash 对账走 Rust 语义口径（episodes.observed_at 不参与哈希——fixture
    // 的 _note_input_hash 留了 Python 含观察时刻的旧值态）。
    assert_eq!(bundle.input_hash, expected["input_hash"].as_str().unwrap());
}

// ---------------------------------------------------------------------------
// build_audience_input（Python tools.build_audience_input：索引 + 两级封顶）
// ---------------------------------------------------------------------------

#[test]
fn audience_input_small_matches_python() {
    let case = load("audience_input_case.json");
    let viewer_map = case["viewer_map"].as_object().unwrap();
    let built =
        build_audience_input(&case["analysis"], viewer_map, &case["graph"]).expect("index builds");
    assert_eq!(built, load("audience_input_expected.json"));
}

/// 构造规则镜像 target/gen_m4a_golden.py 的 build_capped_inputs（键序逐字对齐）。
fn capped_spec_inputs(
    prefix: &str,
    viewer_count: usize,
    state_count: usize,
    summary_len: usize,
) -> (Value, serde_json::Map<String, Value>, Value) {
    let analysis = json!({
        "summary": {},
        "platform_snapshot": {},
        "streamer": {},
        "viewer_profiles": (0..viewer_count)
            .map(|i| json!({"viewer": {"id": format!("{prefix}{i}"), "name": format!("n{i}")}}))
            .collect::<Vec<_>>(),
    });
    let mut viewer_map = serde_json::Map::new();
    for i in 0..viewer_count {
        let uid = format!("{prefix}{i}");
        viewer_map.insert(
            uid.clone(),
            json!({
                "viewer_id": uid,
                "profile_summary": "概".repeat(summary_len),
                "mentions": [],
                "entities": [],
            }),
        );
    }
    let graph = json!({
        "stats": {},
        "nodes": [],
        "edges": [],
        "mentions": [],
        "communities": [],
        "interest_states": (0..state_count)
            .map(|i| json!({
                "state_id": format!("s{i}"),
                "viewer_id": format!("{prefix}{}", i % viewer_count),
                "entity": "实体",
                "status": "稳定",
                "rationale": "长".repeat(9000),
                "confidence": 0.5,
            }))
            .collect::<Vec<_>>(),
    });
    (analysis, viewer_map, graph)
}

#[test]
fn audience_input_cap_tiers_match_python() {
    for tier in load("audience_input_caps.json").as_array().unwrap() {
        let name = tier["name"].as_str().unwrap();
        let (analysis, viewer_map, graph) = capped_spec_inputs(
            tier["prefix"].as_str().unwrap(),
            tier["viewer_count"].as_u64().unwrap() as usize,
            tier["state_count"].as_u64().unwrap() as usize,
            tier["summary_len"].as_u64().unwrap() as usize,
        );
        let expected = &tier["expected"];
        match build_audience_input(&analysis, &viewer_map, &graph) {
            Err(err) => {
                assert!(
                    expected["raised"].as_bool().unwrap(),
                    "{name}: unexpected {err}"
                );
                assert_eq!(
                    err.to_string(),
                    expected["message"].as_str().unwrap(),
                    "{name}"
                );
            }
            Ok(built) => {
                assert!(
                    !expected["raised"].as_bool().unwrap(),
                    "{name}: should not build"
                );
                assert_eq!(
                    built["omitted_interest_state_count"], expected["omitted_interest_state_count"],
                    "{name} omitted"
                );
                assert_eq!(
                    built["interest_state_index"].as_array().unwrap().len() as u64,
                    expected["interest_state_index_count"].as_u64().unwrap(),
                    "{name} included"
                );
                let blanked = built["viewer_index"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|v| v["profile_summary"] == "");
                assert_eq!(
                    blanked,
                    expected["blanked_all_summaries"].as_bool().unwrap(),
                    "{name} blanked"
                );
                assert_eq!(
                    built["viewer_index"].as_array().unwrap().len() as u64,
                    expected["viewer_index_count"].as_u64().unwrap(),
                    "{name} viewer count"
                );
                // Python len() 是字符数；compact_json().chars().count() 对账。
                assert_eq!(
                    canonical_json(&built).chars().count() as u64,
                    expected["payload_chars"].as_u64().unwrap(),
                    "{name} chars"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// aggregate_runtime_usage（Python ai_data.aggregate_runtime_usage：五键重映射求和）
// ---------------------------------------------------------------------------

#[test]
fn aggregate_runtime_usage_matches_python() {
    let viewer_runtime = vec![
        json!({"llm_calls": 2, "tool_calls": 5, "input_tokens": 100, "output_tokens": 40, "total_tokens": 140}),
        json!({"llm_calls": 1, "tool_calls": 3, "input_tokens": 50, "output_tokens": 10, "total_tokens": 60}),
    ];
    let overall = json!({"llm_calls": 3, "tool_calls": 9, "input_tokens": 700, "output_tokens": 300, "total_tokens": 1000});
    assert_eq!(
        aggregate_runtime_usage(&viewer_runtime, &overall),
        load("usage_expected.json")
    );
}

// ---------------------------------------------------------------------------
// compact_interest_state（Python tools._compact_interest_state：None/空串/空数组 剔除）
// ---------------------------------------------------------------------------

#[test]
fn compact_interest_state_drop_table_matches_python() {
    for case in load("compact_state_cases.json").as_array().unwrap() {
        assert_eq!(
            compact_interest_state(&case["in"]),
            case["out"],
            "case {}",
            case["name"].as_str().unwrap()
        );
    }
}

// ---------------------------------------------------------------------------
// golden oracle：哈希身份稳定面（事实不变 → 哈希不变 → AI 零成本复用）。
// 与 *_matches_python parity 钉互补：那些钉「跨语言逐字节一致」，这里钉
// 「观察时刻类过程值漂移永不翻面哈希、真实事实漂移必须翻面」的语义收口。
// ---------------------------------------------------------------------------

#[test]
fn oracle_viewer_hash_immune_to_collected_at_drift() {
    let expected = load("bundle_expected.json");
    let baseline = load("baseline_expected.json");
    let profile = baseline["viewer_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["viewer"]["id"] == "g1")
        .unwrap()
        .clone();
    let viewer: Value = serde_json::from_str(
        &fs::read_to_string(m4a().join("viewer_root/viewers/g1.json")).unwrap(),
    )
    .unwrap();
    let settings = &expected["settings"];
    let rules: Vec<String> = settings["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect();
    let args = |v: &Value| {
        viewer_input_bundle(
            v,
            &profile,
            settings["model"].as_str().unwrap(),
            settings["api"].as_str().unwrap(),
            &settings["reasoning"],
            &rules,
            MAX_EVIDENCE,
        )
    };
    let bundle_a = args(&viewer);

    // 同事实重采：仅采集墙钟 collected_at 漂移（→ episode.observed_at）。
    let mut drifted = viewer.clone();
    drifted["collected_at"] = json!("2099-01-01T00:00:00+00:00");
    let bundle_b = args(&drifted);

    // 前提核验：两批 episode 摘除 observed_at 后逐字节一致（事实确实没变）。
    let strip = |episodes: &[live_core::episodes::Episode]| -> Value {
        let mut value = serde_json::to_value(episodes).unwrap();
        for episode in value.as_array_mut().unwrap() {
            episode.as_object_mut().unwrap().remove("observed_at");
        }
        value
    };
    assert_eq!(
        strip(&bundle_a.episodes),
        strip(&bundle_b.episodes),
        "前提：仅观察时刻漂移，事实面必须逐字节一致"
    );
    // 收口一：哈希不变（complete_cache 跨采集真存活）。
    assert_eq!(
        bundle_a.input_hash, bundle_b.input_hash,
        "oracle：observed_at 漂移不许翻面 input_hash"
    );
    // 收口二：提示面保真——摘口径不摘信息，LLM 仍看得到观察时刻。
    assert!(
        bundle_b
            .input_payload
            .to_string()
            .contains("2099-01-01T00:00:00+00:00"),
        "oracle：payload 必须保留 observed_at（账只摘口径不摘信息）"
    );
}

#[test]
fn oracle_audience_hash_strips_process_metrics() {
    let case = load("audience_input_case.json");
    let viewer_map = case["viewer_map"].as_object().unwrap();
    let built =
        build_audience_input(&case["analysis"], viewer_map, &case["graph"]).expect("index builds");
    let hash_of = |input: &Value| {
        stable_hash(&json!({
            "runtime": "openai-agents",
            "model": "m",
            "api": "chat_completions",
            "input": audience_input_hash_material(input),
        }))
    };
    let baseline_hash = hash_of(&built);

    // 过程指标 + captured_at 漂移 → 哈希不变（situation 缓存跨采集存活的语法）。
    let mut drifted = built.clone();
    drifted["baseline_summary"]["collection_request_count"] = json!(777);
    drifted["baseline_summary"]["collection_elapsed_seconds"] = json!(12.34);
    drifted["platform_snapshot"]["captured_at"] = json!("2099-01-01T00:00:00+00:00");
    assert_eq!(
        hash_of(&drifted),
        baseline_hash,
        "oracle：过程指标/观察时刻漂移不许翻面 situation input_hash"
    );

    // 真实事实面漂移 → 哈希必须翻面（该重判就得重判）。
    let mut factual = built.clone();
    factual["platform_snapshot"]["hot_searches"] = json!(["全新热搜事实"]);
    assert_ne!(
        hash_of(&factual),
        baseline_hash,
        "oracle：事实面漂移必须翻面 situation input_hash"
    );
}

#[test]
fn oracle_oai_usage_cache_fields_round_trip() {
    // 观测盒数据源：DeepSeek wire 字段必须反序列化进 OaiUsage，
    // 非 DeepSeek/无计数字段（旧供应商、demo）缺省归零——不炸旧面。
    let usage: OaiUsage = serde_json::from_str(
        r#"{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_cache_hit_tokens":64,"prompt_cache_miss_tokens":36}"#,
    )
    .unwrap();
    assert_eq!(usage.prompt_cache_hit_tokens, 64);
    assert_eq!(usage.prompt_cache_miss_tokens, 36);
    let legacy: OaiUsage =
        serde_json::from_str(r#"{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}"#)
            .unwrap();
    assert_eq!(
        (
            legacy.prompt_cache_hit_tokens,
            legacy.prompt_cache_miss_tokens
        ),
        (0, 0),
        "无 cache 计数字段的响应必须按零落账"
    );
}

// ---------------------------------------------------------------------------
// 壳（shell_hash + episode_set_hash）双轨身份面。
// design-Δ：Python 冻结面无此概念——input_hash 维持与 m4a golden 逐字对账；
// 两新键的期望真值由本 Rust 公式首批产出，登记注记 fixture（G2-A 白名单先例）。
// ---------------------------------------------------------------------------

/// g1 golden 的 bundle 构造（两测试共用：parity 钉与双轨钉）。
fn g1_bundle() -> live_core::agent::pipeline::ViewerInputBundle {
    let expected = load("bundle_expected.json");
    let baseline = load("baseline_expected.json");
    let profile = baseline["viewer_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["viewer"]["id"] == "g1")
        .unwrap()
        .clone();
    let viewer: Value = serde_json::from_str(
        &fs::read_to_string(m4a().join("viewer_root/viewers/g1.json")).unwrap(),
    )
    .unwrap();
    let settings = &expected["settings"];
    let rules: Vec<String> = settings["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect();
    viewer_input_bundle(
        &viewer,
        &profile,
        settings["model"].as_str().unwrap(),
        settings["api"].as_str().unwrap(),
        &settings["reasoning"],
        &rules,
        MAX_EVIDENCE,
    )
}

#[test]
fn viewer_dual_rail_hashes_match_note_fixture() {
    let note = load("bundle_hashes_note.json");
    let bundle = g1_bundle();
    assert_eq!(bundle.shell_hash, note["shell_hash"].as_str().unwrap());
    assert_eq!(
        bundle.episode_set_hash,
        note["episode_set_hash"].as_str().unwrap()
    );
    // parity 不受污染：整包哈希仍逐字钉 m4a golden。
    let expected = load("bundle_expected.json");
    assert_eq!(bundle.input_hash, expected["input_hash"].as_str().unwrap());
}

#[test]
fn oracle_hash_scopes_track_changes() {
    let bundle = g1_bundle();
    // 成员重排（同事实乱序）→ 集合身份不翻面。
    let reversed: Vec<_> = bundle.episodes.iter().rev().cloned().collect();
    assert_eq!(
        episode_set_hash(&reversed),
        bundle.episode_set_hash,
        "集合身份必须序无关"
    );
    // 同一 viewer 事实变更→ 壳不动，集/整包双翻面。
    // mutation 点 = baseline evidence_catalog（上游采集工程化裁剪后的事实面，
    // bundle 消费此口径——改 raw viewer.sources 不会传导，见后记）。
    let expected = load("bundle_expected.json");
    let baseline = load("baseline_expected.json");
    let profile = baseline["viewer_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["viewer"]["id"] == "g1")
        .unwrap()
        .clone();
    let mut edited_profile = profile.clone();
    edited_profile["evidence_catalog"][0]["title"] = json!("全新标题——事实变更");
    let viewer: Value = serde_json::from_str(
        &fs::read_to_string(m4a().join("viewer_root/viewers/g1.json")).unwrap(),
    )
    .unwrap();
    let settings = &expected["settings"];
    let rules: Vec<String> = settings["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect();
    let edited_bundle = viewer_input_bundle(
        &viewer,
        &edited_profile,
        settings["model"].as_str().unwrap(),
        settings["api"].as_str().unwrap(),
        &settings["reasoning"],
        &rules,
        MAX_EVIDENCE,
    );
    assert_eq!(
        edited_bundle.shell_hash, bundle.shell_hash,
        "壳身份 = 环境+描述子，episode 漂移不翻面"
    );
    assert_ne!(
        edited_bundle.episode_set_hash, bundle.episode_set_hash,
        "集合身份必须捕到内容变更（episode_id 焊 content_version）"
    );
    assert_ne!(
        edited_bundle.input_hash, bundle.input_hash,
        "整包口径随内容变更翻面（Z5 parity 语义不动）"
    );
}
