//! M4-A golden 对账：pipeline 输入小件 5 件套 与 Python 预言机真值 fixtures 比对。
//! fixtures = tests-fixtures/m4a/（由私仓 target/gen_m4a_golden.py 调 Python 实算产出）。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use live_core::agent::pipeline::{
    aggregate_runtime_usage, build_audience_input, canonical_json, compact_interest_state,
    stable_hash, viewer_input_bundle,
};
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
