//! M4-D 合成 demo 通道（Python demo.py 平移，D-5 裁剪：不产 peers/* 与 HTML 站）。
//!
//! 与真实采集 schema 完全一致的合成三观众（demo-1/2 共享「异环」→ SAME_AS）→
//! baseline → Episode → 合成终局提交 → 图应用 → project 取回实体/提及 ID →
//! 合成 overall → apply_audience → complete_run → ai/* 落盘。全程零网络零 LLM。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use thiserror::Error;

use crate::config::Config;
use crate::episodes::Episode;
use crate::episodes::baseline::{build_factual_baseline, viewer_context};
use crate::episodes::episodes_from_context;
use crate::graph::build::{apply_audience_submission, apply_viewer_submission};
use crate::graph::project::{self, AUDIENCE_GRAPH_LIMIT, ProjectOptions};
use crate::graph::query::search_entities;
use crate::graph::store::Store;
use crate::models::{
    AudienceCommunity, AudienceSituationSubmission, ContentOpportunity, EntityProposal,
    GraphInterestItem, InterestStateProposal, MentionSpan, SituationItem,
    ViewerPerceptionSubmission,
};
use crate::storage;

/// Python demo.py 的模型落款常量：合成通道不经过真实 LLM，analysis 可解释性
/// 依赖「model=synthetic-demo」与输入侧 status=complete 缓存形状（_complete_cache 可复用）。
pub const SYNTHETIC_MODEL: &str = "synthetic-demo";
/// 合成快照/时间戳字面量（Python demo.py 字面 parity；确定性是两次跑对账的前提）。
const DEMO_TIMESTAMP: &str = "2026-07-12T08:00:00+00:00";
/// 收藏条目发布日字面（Python demo.py:82 独立字面，与 DEMO_TIMESTAMP 不同日 ——
/// 误并常量会经 episode content_version 让全链 ID 走样）。
const FAVORITE_PUBLISHED_AT: &str = "2026-07-10T00:00:00+00:00";
/// 合成 mention/entity/state 置信度字面量（Python demo.py:122/155/167）。
const MENTION_CONFIDENCE: f64 = 0.96;
const ENTITY_CONFIDENCE: f64 = 0.94;
const STATE_CONFIDENCE: f64 = 0.86;

#[derive(Debug, Error)]
pub enum DemoError {
    #[error("{0}")]
    Message(String),
    /// storage 模块错误面是 String（StorageResult）；此处仅换个带类名的外衣。
    #[error("storage: {0}")]
    Storage(String),
    #[error("store: {0}")]
    Store(#[from] crate::graph::store::StoreError),
}

/// `storage::write_json` 的 `?` 桥（StorageResult<T> = Result<T, String>）。
fn demo_write(path: &Path, value: &Value) -> Result<(), DemoError> {
    storage::write_json(path, value).map_err(DemoError::Storage)
}

/// Python demo.py `output`/`overall` 是字面 dict：键序即书写序、无 `leads` 键（模型面
/// 后加的字段不进合成字面）。ai/* 落盘分析体必须与该形状逐键相等（Python 是预言机），
/// 因此 typed → Value 后按 Python 键序重建对象。
fn python_order<T: serde::Serialize>(submission: &T, order: &[&str]) -> Value {
    let value = serde_json::to_value(submission).expect("submission 可序列化");
    let map = value.as_object().expect("顶层是对象");
    let mut out = serde_json::Map::new();
    for key in order {
        if let Some(entry) = map.get(*key) {
            out.insert((*key).to_string(), entry.clone());
        }
    }
    Value::Object(out)
}

/// Python demo.py `_viewer_output` 返回 dict 的键序（leads 不在字面中）。
const VIEWER_KEYS: &[&str] = &[
    "viewer_id",
    "profile_summary",
    "mentions",
    "entities",
    "relations",
    "interest_states",
    "content_preferences",
    "recent_changes",
    "hypotheses",
    "conversation_openers",
    "content_ideas",
    "enrichment_targets",
    "cautions",
];
/// Python demo.py `overall` 字面键序。
const AUDIENCE_KEYS: &[&str] = &[
    "executive_summary",
    "audience_structure",
    "interest_graph",
    "communities",
    "situations",
    "content_opportunities",
    "individual_highlights",
    "content_calendar",
    "data_gaps",
    "safety_notes",
];

impl From<crate::episodes::baseline::BaselineError> for DemoError {
    fn from(error: crate::episodes::baseline::BaselineError) -> Self {
        DemoError::Message(error.to_string())
    }
}

fn empty_source() -> Value {
    json!({"status": "empty", "count": 0, "detail": "", "items": []})
}

fn favorite(
    item_id: &str,
    title: &str,
    description: &str,
    tags: &[&str],
    creator: &str,
    category: &str,
) -> Value {
    json!({
        "id": item_id,
        "source": "favorite",
        "title": title,
        "description": description,
        "published_at": FAVORITE_PUBLISHED_AT,
        "url": "",
        "bvid": "",
        "tags": tags,
        "platform_category": {"id": 172, "name": category},
        "creator_id": format!("creator-{item_id}"),
        "creator_name": creator,
        "folder_name": "演示收藏",
    })
}

fn viewer(uid: &str, name: &str, items: Value) -> Value {
    let item_count = items.as_array().map_or(0, Vec::len);
    json!({
        "schema_version": 1,
        "collected_at": DEMO_TIMESTAMP,
        "viewer": {
            "id": uid,
            "name": name,
            "face": "",
            "profile_url": format!("https://space.bilibili.com/{uid}"),
            "guard_level": 3,
            "medal_level": 20,
            "seed_source": "demo",
        },
        "profile": {
            "uid": uid,
            "name": name,
            "face": "",
            "sign": "",
            "official_title": "",
            "level": 5,
            "following": 0,
            "followers": 0,
            "profile_url": format!("https://space.bilibili.com/{uid}"),
        },
        "sources": {
            "profile": {"status": "ok", "count": 1, "detail": ""},
            "relation_stat": {"status": "ok", "count": 1, "detail": ""},
            "followings": empty_source(),
            "videos": empty_source(),
            "dynamics": empty_source(),
            "favorites": {
                "status": "ok",
                "count": item_count,
                "detail": "synthetic demo",
                "folders": [],
                "items": items,
            },
            "bangumi": empty_source(),
            "games": empty_source(),
            "coins": {"status": "unsupported", "count": 0, "detail": "", "items": []},
            "likes": {"status": "unsupported", "count": 0, "detail": "", "items": []},
        },
    })
}

/// `demo.py _viewer` 的三观众字面集（次序即 Python 字面次序）。
fn demo_viewers() -> Vec<Value> {
    vec![
        viewer(
            "demo-1",
            "演示观众A",
            json!([favorite(
                "a1",
                "《异环》实机演示：都市探索与角色演出",
                "关注城市空间、演出和开放世界体验。",
                &["异环", "开放世界", "实机演示"],
                "演示UP主甲",
                "手机游戏",
            )]),
        ),
        viewer(
            "demo-2",
            "演示观众B",
            json!([favorite(
                "b1",
                "《异环》城市设计解析与《明日方舟》世界观讨论",
                "喜欢世界观、城市设计与剧情解析。",
                &["异环", "明日方舟", "剧情解析"],
                "演示UP主乙",
                "手机游戏",
            )]),
        ),
        viewer(
            "demo-3",
            "演示观众C",
            json!([favorite(
                "c1",
                "Vocaloid角色曲MV与Blender动画制作流程",
                "关注虚拟歌手、音乐视觉化和三维动画创作。",
                &["Vocaloid", "Blender", "动画制作"],
                "演示UP主丙",
                "音乐综合",
            )]),
        ),
    ]
}

/// spec 行：text=待定位字面 / name、type=实体名与类型 / status（None → Python 默认
/// 「近期上升」，demo.py `item.get("status", "近期上升")`）/ aspect（Some → state.aspects）。
struct ViewerSpec {
    text: &'static str,
    name: &'static str,
    entity_type: &'static str,
    status: Option<&'static str>,
    aspect: Option<&'static str>,
}

fn demo_specs(uid: &str) -> Vec<ViewerSpec> {
    match uid {
        "demo-1" => vec![
            ViewerSpec {
                text: "异环",
                name: "异环",
                entity_type: "game",
                status: None,
                aspect: Some("都市探索"),
            },
            ViewerSpec {
                text: "角色演出",
                name: "角色演出",
                entity_type: "content_aspect",
                status: Some("稳定"),
                aspect: None,
            },
        ],
        "demo-2" => vec![
            ViewerSpec {
                text: "异环",
                name: "异环",
                entity_type: "game",
                status: None,
                aspect: Some("城市设计"),
            },
            ViewerSpec {
                text: "明日方舟",
                name: "明日方舟",
                entity_type: "game",
                status: Some("稳定"),
                aspect: None,
            },
        ],
        _ => vec![
            ViewerSpec {
                text: "Vocaloid",
                name: "Vocaloid",
                entity_type: "music_culture",
                status: Some("核心"),
                aspect: None,
            },
            ViewerSpec {
                text: "Blender",
                name: "Blender",
                entity_type: "software",
                status: None,
                aspect: Some("动画制作"),
            },
        ],
    }
}

/// `demo.py _mention`：episodes×fields 顺序首个包含 text 的字段（Python str.find 语义：
/// 字节码点混合时返回字符序索引——fields 文本为 UTF-8，需 chars 定位）。
fn find_mention(
    episodes: &[Episode],
    mention_id: &str,
    spec: &ViewerSpec,
) -> Result<MentionSpan, DemoError> {
    for episode in episodes {
        for field in &episode.fields {
            if let Some(byte_index) = field.text.find(spec.text) {
                let start = field.text[..byte_index].chars().count() as i64;
                let end = start + spec.text.chars().count() as i64;
                return Ok(MentionSpan {
                    mention_id: mention_id.to_string(),
                    episode_id: episode.episode_id.clone(),
                    field_path: field.path.clone(),
                    text: spec.text.to_string(),
                    start,
                    end,
                    mention_type: spec.entity_type.to_string(),
                    origin: if field.kind.starts_with("platform_") {
                        "platform".to_string()
                    } else {
                        "explicit".to_string()
                    },
                    proposed_entity_name: spec.name.to_string(),
                    proposed_entity_type: spec.entity_type.to_string(),
                    entity_ref: format!("entity:{}", mention_id.replace('m', "e")),
                    confidence: MENTION_CONFIDENCE,
                });
            }
        }
    }
    Err(DemoError::Message(format!(
        "demo mention not found: {}",
        spec.text
    )))
}

/// `demo.py _viewer_output`：mentions/entities/states 三段平移；resolution 一律 NEW_ENTITY
///（demo-2 的「异环」SAME_AS 改写发生在应用环——build_demo 内，与 Python 同位）。
fn viewer_output(
    viewer_id: &str,
    episodes: &[Episode],
    specs: &[ViewerSpec],
) -> Result<ViewerPerceptionSubmission, DemoError> {
    let mut mentions = Vec::with_capacity(specs.len());
    let mut entities = Vec::with_capacity(specs.len());
    let mut states = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let ordinal = index + 1;
        let mention_id = format!("m{ordinal}");
        let entity_ref = format!("entity:e{ordinal}");
        mentions.push(find_mention(episodes, &mention_id, spec)?);
        entities.push(EntityProposal {
            local_id: format!("e{ordinal}"),
            canonical_name: spec.name.to_string(),
            entity_type: spec.entity_type.to_string(),
            aliases: Vec::new(),
            description: String::new(),
            existing_entity_id: None,
            resolution: "NEW_ENTITY".to_string(),
            evidence_mention_ids: vec![mention_id.clone()],
            parent_entity_refs: Vec::new(),
            confidence: ENTITY_CONFIDENCE,
        });
        states.push(InterestStateProposal {
            entity_ref,
            status: spec.status.unwrap_or("近期上升").to_string(),
            preference: "关注具体内容与讨论角度".to_string(),
            aspects: spec.aspect.iter().map(|text| (*text).to_string()).collect(),
            rationale: "演示数据中的公开收藏和平台标签形成可追溯证据。".to_string(),
            evidence_mention_ids: vec![mention_id],
            confidence: STATE_CONFIDENCE,
        });
    }
    Ok(ViewerPerceptionSubmission {
        viewer_id: viewer_id.to_string(),
        profile_summary: "演示：从公开Episode中开放式识别具体实体，并形成可追溯兴趣状态。"
            .to_string(),
        mentions,
        entities,
        relations: Vec::new(),
        interest_states: states,
        content_preferences: vec!["具体作品讨论".to_string(), "可验证素材".to_string()],
        recent_changes: vec!["出现新的具体作品关注信号".to_string()],
        hypotheses: vec!["需要下一次快照验证兴趣是否持续".to_string()],
        conversation_openers: Vec::new(),
        content_ideas: Vec::new(),
        enrichment_targets: Vec::new(),
        cautions: vec!["这是合成Demo，不代表真实观众。".to_string()],
        leads: Vec::new(),
    })
}

/// `demo.py build_demo`（D-5 裁剪 peers/HTML 后）+ D-6：默认输出根 = `output_dir 同级/_demo`。
///
/// 返回值 = Python build_demo dict 的 D-5 裁剪版（无 report 键）：
/// `{status, synthetic_demo, output_dir, graph:{database}}`。
pub fn build_demo(config: &Config, output_dir: Option<&Path>) -> Result<Value, DemoError> {
    let demo_root: PathBuf = match output_dir {
        Some(dir) => dir.to_path_buf(),
        None => config
            .output_dir
            .parent()
            .unwrap_or(Path::new("."))
            .join("_demo"),
    };
    let mut demo_config = config.clone();
    demo_config.output_dir = demo_root.clone();
    let max_evidence = demo_config.perception.max_evidence_per_viewer as usize;

    let viewers = demo_viewers();
    demo_write(
        &demo_root.join("collection.json"),
        &json!({
            "status": "complete",
            "viewer_count": viewers.len(),
            "request_count": 0,
            "elapsed_seconds": 0,
            "synthetic_demo": true,
        }),
    )?;
    demo_write(
        &demo_root.join("streamer.json"),
        &json!({"synthetic_demo": true, "sources": {}}),
    )?;
    demo_write(
        &demo_root.join("shared").join("platform_snapshot.json"),
        &json!({
            "snapshot_type": "bilibili_platform",
            "captured_at": DEMO_TIMESTAMP,
            "synthetic_demo": true,
            "hot_searches": ["异环", "明日方舟"],
        }),
    )?;
    for item in &viewers {
        let uid = item["viewer"]["id"].as_str().unwrap_or_default();
        demo_write(&demo_root.join("viewers").join(format!("{uid}.json")), item)?;
    }

    // `demo.py`：baseline + viewer_context + episodes 全通道消费（不绕过事实整理环）。
    let baseline = build_factual_baseline(&demo_root, max_evidence)?;
    let profiles = baseline["viewer_profiles"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut outputs = Vec::with_capacity(viewers.len());
    let mut episode_sets: Vec<(String, Vec<Episode>)> = Vec::with_capacity(viewers.len());
    for item in &viewers {
        let uid = item["viewer"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let profile = profiles
            .iter()
            .find(|p| p["viewer"]["id"].as_str() == Some(uid.as_str()))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let context = viewer_context(item, &profile, max_evidence);
        let episodes = episodes_from_context(&context);
        let output = viewer_output(&uid, &episodes, &demo_specs(&uid))?;
        episode_sets.push((uid, episodes));
        outputs.push((profile, output));
    }

    // 图环：begin_run → 逐观众 apply（demo-2 「异环」改写 SAME_AS）→ project 取回
    // 实体/提及 ID → 合成 overall → apply_audience → complete_run。
    // Python 失败路径 = fail_run(aborted=非 Exception 型)；Rust DemoError 全族等价
    // Exception → 恒 aborted=false（无 KeyboardInterrupt 同型物）。
    let store = Store::open(&demo_root.join("graph").join("perception.sqlite3"))?;
    let run_id = store.begin_run(SYNTHETIC_MODEL)?;
    let graph_stage = (|| -> Result<AudienceSituationSubmission, DemoError> {
        let mut shared_entity_id = String::new();
        for (profile, output) in &mut outputs {
            let uid = &output.viewer_id;
            if uid == "demo-2" {
                let shared = output
                    .entities
                    .iter_mut()
                    .find(|entity| entity.canonical_name == "异环")
                    .expect("demo-2 specs 含异环");
                shared.resolution = "SAME_AS".to_string();
                shared.existing_entity_id = Some(shared_entity_id.clone());
            }
            let episodes = &episode_sets
                .iter()
                .find(|(id, _)| id == uid)
                .expect("episode set 与 output 同序")
                .1;
            let viewer_name = profile["viewer"]["name"].as_str().unwrap_or_default();
            apply_viewer_submission(&store, &run_id, viewer_name, episodes, output)?;
            if uid == "demo-1" {
                shared_entity_id = search_entities(&store, "异环", "game", 100)?
                    .first()
                    .and_then(|row| row["entity_id"].as_str())
                    .unwrap_or_default()
                    .to_string();
            }
        }

        let graph = project::project(
            &store,
            &ProjectOptions {
                include_episodes: false,
                include_interest_states: false,
                limit: Some(AUDIENCE_GRAPH_LIMIT),
                minimum_community_size: demo_config.perception.minimum_community_size,
                ..ProjectOptions::default()
            },
        )?;
        // Python entity_by_name 是 dict 推导：同名 Entity 节点「后者覆盖」
        // （ORDER BY node_type,name 下 tag 先于 ai game——异环取 game）。
        let entity_id_of = |name: &str| -> String {
            graph["nodes"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .rfind(|node| {
                    node["type"].as_str() == Some("Entity") && node["name"].as_str() == Some(name)
                })
                .and_then(|node| node["id"].as_str())
                .unwrap_or_default()
                .to_string()
        };
        let mut yihuan_mentions: Vec<String> = graph["mentions"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter(|row| row["text"].as_str() == Some("异环"))
            .filter_map(|row| row["mention_id"].as_str().map(str::to_string))
            .collect();

        let overall = AudienceSituationSubmission {
            executive_summary:
                "合成Demo显示：两名观众通过不同内容角度连接到《异环》，另一名观众形成Vocaloid与Blender创作型独立兴趣。"
                    .to_string(),
            audience_structure: vec![
                "具体作品共同兴趣".to_string(),
                "创作型独立兴趣".to_string(),
            ],
            interest_graph: vec![GraphInterestItem {
                entity_id: entity_id_of("异环"),
                entity: "异环".to_string(),
                entity_type: "game".to_string(),
                parent_entities: Vec::new(),
                angles: vec![
                    "都市探索".to_string(),
                    "城市设计".to_string(),
                    "角色演出".to_string(),
                ],
                viewer_ids: vec!["demo-1".to_string(), "demo-2".to_string()],
                status: "近期上升".to_string(),
                confidence: 0.9,
                evidence_summary: "两名观众的独立公开收藏均出现该实体。".to_string(),
                evidence_mention_ids: yihuan_mentions.clone(),
            }],
            communities: vec![AudienceCommunity {
                name: "异环城市与演出讨论群".to_string(),
                description: "共同作品一致，但关注角度不同。".to_string(),
                viewer_ids: vec!["demo-1".to_string(), "demo-2".to_string()],
                entity_ids: vec![entity_id_of("异环")],
                entities: vec!["异环".to_string()],
                shared_angles: vec!["都市空间".to_string(), "演出".to_string()],
                evidence_mention_ids: yihuan_mentions.clone(),
                confidence: 0.88,
            }],
            situations: vec![SituationItem {
                title: "《异环》形成跨观众共同讨论入口".to_string(),
                status: "新出现".to_string(),
                description: "同一具体作品通过城市设计和角色演出两个角度连接核心观众。"
                    .to_string(),
                entity_ids: vec![entity_id_of("异环")],
                entities: vec!["异环".to_string()],
                viewer_ids: vec!["demo-1".to_string(), "demo-2".to_string()],
                trigger_events: vec!["两条独立公开收藏".to_string()],
                evidence_mention_ids: yihuan_mentions.clone(),
                confidence: 0.9,
                recommended_investigation: vec![
                    "下一次快照确认是否继续出现相关内容".to_string(),
                ],
            }],
            content_opportunities: vec![ContentOpportunity {
                title: "《异环》城市体验与角色演出讨论".to_string(),
                entity_id: entity_id_of("异环"),
                entity: "异环".to_string(),
                why_now: "多个核心观众出现同一具体作品信号。".to_string(),
                why_fit: "可同时覆盖城市探索和角色演出两个关注角度。".to_string(),
                audience_ids: vec!["demo-1".to_string(), "demo-2".to_string()],
                format: "素材观看 + 观点投票".to_string(),
                run_of_show: vec![
                    "说明素材来源".to_string(),
                    "观看关键片段".to_string(),
                    "分别讨论城市与演出".to_string(),
                    "记录互动反馈".to_string(),
                ],
                talking_points: vec![
                    "城市空间是否有辨识度".to_string(),
                    "角色演出是否自然".to_string(),
                ],
                evidence_mention_ids: std::mem::take(&mut yihuan_mentions),
                search_result_ids: Vec::new(),
                confidence: "高".to_string(),
                observation_metrics: vec![
                    "相关观众发言".to_string(),
                    "停留时长".to_string(),
                    "投票参与".to_string(),
                ],
                caveats: vec!["Demo为合成数据，不代表真实观众判断".to_string()],
            }],
            individual_highlights: Vec::new(),
            content_calendar: Vec::new(),
            data_gaps: vec!["需要第二次快照验证趋势".to_string()],
            safety_notes: vec!["只展示公开事实和可追溯推断".to_string()],
            leads: Vec::new(),
        };
        apply_audience_submission(&store, &run_id, &overall)?;
        store.complete_run(&run_id)?;
        Ok(overall)
    })();
    let overall = match graph_stage {
        Ok(overall) => overall,
        Err(error) => {
            store.fail_run(&run_id, &error.to_string(), false)?;
            return Err(error);
        }
    };

    for (_, output) in &outputs {
        demo_write(
            &demo_root
                .join("ai")
                .join("perception")
                .join("viewers")
                .join(format!("{}.json", output.viewer_id)),
            &json!({
                "status": "complete",
                "input_hash": SYNTHETIC_MODEL,
                "model": SYNTHETIC_MODEL,
                "protocol": "terminal_tool_call",
                "terminal_tool": "submit_viewer_perception",
                "analysis": python_order(output, VIEWER_KEYS),
            }),
        )?;
    }
    demo_write(
        &demo_root.join("ai").join("situation.json"),
        &json!({
            "status": "complete",
            "input_hash": SYNTHETIC_MODEL,
            "model": SYNTHETIC_MODEL,
            "protocol": "terminal_tool_call",
            "terminal_tool": "submit_audience_situation",
            "analysis": python_order(&overall, AUDIENCE_KEYS),
        }),
    )?;
    demo_write(
        &demo_root.join("ai").join("state.json"),
        &json!({
            "status": "complete",
            "model": SYNTHETIC_MODEL,
            "protocol": "tool_call_only",
            "situation_input_hash": SYNTHETIC_MODEL,
            "viewer_input_hashes": outputs
                .iter()
                .map(|(_, output)| (output.viewer_id.clone(), Value::from(SYNTHETIC_MODEL)))
                .collect::<serde_json::Map<String, Value>>(),
            "graph_run_id": run_id,
            "synthetic_demo": true,
        }),
    )?;
    Ok(json!({
        "status": "complete",
        "synthetic_demo": true,
        "output_dir": demo_root.to_string_lossy(),
        "graph": {"database": demo_root.join("graph").join("perception.sqlite3").to_string_lossy()},
    }))
}
