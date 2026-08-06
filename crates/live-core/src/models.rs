//! Agent 终局提交的数据模型（移植 Python `ai_models.py`）。
//!
//! 单点定义：serde 解析 + schemars 工具参数 schema 都从这里派生（设计文档 M1：
//! models.rs 是工具参数的唯一事实源）。Python 的 Text200 等长度约束在
//! `agent/validators.rs`（M3）统一校验；本文件只承载结构 + 默认值 + extra=forbid。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_half() -> f64 {
    0.5
}
fn default_origin() -> String {
    "explicit".to_string()
}
fn default_resolution() -> String {
    "NEW_ENTITY".to_string()
}
fn default_status_unknown() -> String {
    "无法判断".to_string()
}
fn default_confidence_word() -> String {
    "低".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MentionSpan {
    /// 本次提交内唯一ID，例如 m1
    pub mention_id: String,
    pub episode_id: String,
    /// Episode中的字段路径，例如 title、description、tags[0]
    pub field_path: String,
    /// 原文中精确出现的文本
    pub text: String,
    pub start: i64,
    pub end: i64,
    /// 开放式Mention类型，例如作品名、游戏名、角色名、创作者、技术、事件
    pub mention_type: String,
    #[serde(default = "default_origin")]
    pub origin: String,
    pub proposed_entity_name: String,
    pub proposed_entity_type: String,
    /// 该Mention直接指向的 entity:<local_id> 或已存在 entity_id
    pub entity_ref: String,
    #[serde(default = "default_half")]
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityProposal {
    /// 本次提交内唯一实体引用，例如 e1
    pub local_id: String,
    pub canonical_name: String,
    /// 开放式实体类型，不受固定分类表限制
    pub entity_type: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// 调用 search_entity_candidates 确认后可指向已有实体；不确定时留空
    #[serde(default)]
    pub existing_entity_id: Option<String>,
    #[serde(default = "default_resolution")]
    pub resolution: String,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
    /// 可引用本次实体 local_id 或已有 entity_id；允许多父关系
    #[serde(default)]
    pub parent_entity_refs: Vec<String>,
    #[serde(default = "default_half")]
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationProposal {
    /// viewer:self、输入中的精确 episode_id、entity:<local_id> 或已有 entity_id
    pub subject_ref: String,
    /// 简洁关系，例如 ABOUT、FOCUSES_ON、RELATED_TO、INSTANCE_OF
    pub predicate: String,
    pub object_ref: String,
    #[serde(default)]
    pub interpretation: String,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
    #[serde(default = "default_half")]
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterestStateProposal {
    /// entity:<local_id> 或已有 entity_id
    pub entity_ref: String,
    #[serde(default = "default_status_unknown")]
    pub status: String,
    #[serde(default)]
    pub preference: String,
    #[serde(default)]
    pub aspects: Vec<String>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
    #[serde(default = "default_half")]
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewerAction {
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
    #[serde(default)]
    pub search_result_ids: Vec<String>,
    #[serde(default)]
    pub observation_metrics: Vec<String>,
    #[serde(default)]
    pub risk: String,
}

/// 采集线索（§M4.x 薄切，G1 提前出生：schema + 校验随 M3 终局协议进入；
/// 程序侧账本/消费在 M4.x）。`type` 白名单与 locator 形态校验在 validators 层。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lead {
    /// search | creator | video | room
    #[serde(rename = "type")]
    pub lead_type: String,
    pub locator: String,
    pub motivation: String,
    pub expected_signal: String,
    pub priority: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

/// 观众级终局提交（`submit_viewer_perception` 的参数）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewerPerceptionSubmission {
    pub viewer_id: String,
    /// Python Text100000：schema 形状对齐（运行时硬拒在 validators 层 SUMMARY_MAX_CHARS）
    #[schemars(length(max = 100_000))]
    pub profile_summary: String,
    #[serde(default)]
    pub mentions: Vec<MentionSpan>,
    #[serde(default)]
    pub entities: Vec<EntityProposal>,
    #[serde(default)]
    pub relations: Vec<RelationProposal>,
    #[serde(default)]
    pub interest_states: Vec<InterestStateProposal>,
    #[serde(default)]
    pub content_preferences: Vec<String>,
    #[serde(default)]
    pub recent_changes: Vec<String>,
    #[serde(default)]
    pub hypotheses: Vec<String>,
    #[serde(default)]
    pub conversation_openers: Vec<ViewerAction>,
    #[serde(default)]
    pub content_ideas: Vec<ViewerAction>,
    #[serde(default)]
    pub enrichment_targets: Vec<String>,
    #[serde(default)]
    pub cautions: Vec<String>,
    #[serde(default)]
    pub leads: Vec<Lead>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphInterestItem {
    #[serde(default)]
    pub entity_id: String,
    pub entity: String,
    pub entity_type: String,
    #[serde(default)]
    pub parent_entities: Vec<String>,
    #[serde(default)]
    pub angles: Vec<String>,
    #[serde(default)]
    pub viewer_ids: Vec<String>,
    #[serde(default = "default_status_unknown")]
    pub status: String,
    #[serde(default = "default_half")]
    pub confidence: f64,
    #[serde(default)]
    pub evidence_summary: String,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudienceCommunity {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub viewer_ids: Vec<String>,
    #[serde(default)]
    pub entity_ids: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub shared_angles: Vec<String>,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
    #[serde(default = "default_half")]
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SituationItem {
    pub title: String,
    /// 上升、稳定、新出现、衰退、待验证等
    pub status: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub entity_ids: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub viewer_ids: Vec<String>,
    #[serde(default)]
    pub trigger_events: Vec<String>,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
    #[serde(default = "default_half")]
    pub confidence: f64,
    #[serde(default)]
    pub recommended_investigation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentOpportunity {
    pub title: String,
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub entity: String,
    #[serde(default)]
    pub why_now: String,
    #[serde(default)]
    pub why_fit: String,
    #[serde(default)]
    pub audience_ids: Vec<String>,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub run_of_show: Vec<String>,
    #[serde(default)]
    pub talking_points: Vec<String>,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
    #[serde(default)]
    pub search_result_ids: Vec<String>,
    /// ConfidenceWord：高/中/低
    #[serde(default = "default_confidence_word")]
    pub confidence: String,
    #[serde(default)]
    pub observation_metrics: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
}

/// ConfidenceWord → 数值（设计文档 §8.1：TARGETS/ABOUT 等 action 边必带 confidence）。
/// 口径：中=0.5 与全库默认中性感一致；高=1.0；低=0.2 显著低于默认线。
pub const CONFIDENCE_WORD_HIGH: f64 = 1.0;
pub const CONFIDENCE_WORD_MEDIUM: f64 = 0.5;
pub const CONFIDENCE_WORD_LOW: f64 = 0.2;

pub fn confidence_word_score(word: &str) -> f64 {
    match word {
        "高" => CONFIDENCE_WORD_HIGH,
        "中" => CONFIDENCE_WORD_MEDIUM,
        _ => CONFIDENCE_WORD_LOW,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndividualHighlight {
    pub viewer_id: String,
    pub insight: String,
    #[serde(default)]
    pub opportunity: String,
    #[serde(default)]
    pub evidence_mention_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentCalendarItem {
    pub session: String,
    pub theme: String,
    #[serde(default)]
    pub target_viewers: Vec<String>,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub validation_signal: String,
}

/// Z5/C1：制片人简报单句——结论句 + 证据 episode 引用 + 覆盖时段（可空）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BriefSentence {
    /// 结论句（非空；长度硬闸门在 validators 层）。
    pub text: String,
    /// 句句带出处（哲学席两颗桩之一）：至少 1 个真实 episode_id；存在性闭包在
    /// specs 终局工具的 graph references 通道（restore 复跑只做结构校验——
    /// 图谱 append-only，提交时存在即永远存在）。
    pub episode_refs: Vec<String>,
    /// 覆盖时段 [from, to]（ISO 日期/时间字符串，from<=to）；可缺席。
    #[serde(default)]
    pub coverage_time_range: Option<[String; 2]>,
}

/// Z5/C1（终裁 P0-5）：front_brief = 制片人简报。结论先行，句句带出处，
/// 沉默可呈现——sentences 空数组合法（前端 BriefingCard 为此呈「空缺位」）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrontBrief {
    #[serde(default)]
    pub sentences: Vec<BriefSentence>,
}

/// 整体态势终局提交（`submit_audience_situation` 的参数）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudienceSituationSubmission {
    /// Python Text100000：schema 形状对齐（运行时硬拒在 validators 层 SUMMARY_MAX_CHARS）
    #[schemars(length(max = 100_000))]
    pub executive_summary: String,
    /// Z5/C1：简报位于 schema 前列——终局参数序即模型阅读序（结论先行语义进 wire）。
    #[serde(default)]
    pub front_brief: FrontBrief,
    #[serde(default)]
    pub audience_structure: Vec<String>,
    #[serde(default)]
    pub interest_graph: Vec<GraphInterestItem>,
    #[serde(default)]
    pub communities: Vec<AudienceCommunity>,
    #[serde(default)]
    pub situations: Vec<SituationItem>,
    #[serde(default)]
    pub content_opportunities: Vec<ContentOpportunity>,
    #[serde(default)]
    pub individual_highlights: Vec<IndividualHighlight>,
    #[serde(default)]
    pub content_calendar: Vec<ContentCalendarItem>,
    #[serde(default)]
    pub data_gaps: Vec<String>,
    #[serde(default)]
    pub safety_notes: Vec<String>,
    #[serde(default)]
    pub leads: Vec<Lead>,
}

/// agent-check 探针提交（Python ai_models.ProbeResult 契约：get_probe_seed /
/// multiply_probe_seed / submit_probe_result 三工具的唯一终局）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    pub a: i64,
    pub b: i64,
    pub total: i64,
    #[serde(default)]
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_viewer_submission() {
        let raw = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/ai/perception/viewers/demo-1.json"),
        )
        .unwrap();
        let wrapper: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let submission: ViewerPerceptionSubmission =
            serde_json::from_value(wrapper["analysis"].clone()).unwrap();
        assert_eq!(submission.viewer_id, "demo-1");
        assert_eq!(submission.mentions.len(), 2);
        assert_eq!(submission.entities.len(), 2);
        assert_eq!(submission.interest_states.len(), 2);
        assert_eq!(submission.mentions[0].confidence, 0.96);
        assert_eq!(submission.entities[0].resolution, "NEW_ENTITY");
    }

    #[test]
    fn parses_fixture_audience_submission() {
        let raw = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests-fixtures/demo/ai/situation.json"),
        )
        .unwrap();
        let wrapper: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let submission: AudienceSituationSubmission =
            serde_json::from_value(wrapper["analysis"].clone()).unwrap();
        assert_eq!(submission.situations.len(), 1);
        assert_eq!(submission.content_opportunities.len(), 1);
        assert_eq!(submission.content_opportunities[0].confidence, "高");
        assert_eq!(
            confidence_word_score(&submission.content_opportunities[0].confidence),
            CONFIDENCE_WORD_HIGH
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let raw = r#"{"viewer_id":"v","profile_summary":"x","bogus":1}"#;
        assert!(serde_json::from_str::<ViewerPerceptionSubmission>(raw).is_err());
    }

    #[test]
    fn word_score_mapping() {
        assert_eq!(confidence_word_score("高"), 1.0);
        assert_eq!(confidence_word_score("中"), 0.5);
        assert_eq!(confidence_word_score("低"), 0.2);
        assert_eq!(confidence_word_score("未知词"), 0.2);
    }

    #[test]
    fn probe_result_and_lead_models() {
        // Python 契约：note 默认 ""（ai_models.py:259 行 family）
        let probe: ProbeResult = serde_json::from_str(r#"{"a":7,"b":14,"total":21}"#).unwrap();
        assert_eq!(probe.note, "");
        // Lead 的 JSON 键是 "type"；leads 在两份终局提交上均默认空
        let lead: Lead = serde_json::from_str(
            r#"{"type":"search","locator":"某位 vtuber 切片","motivation":"m","expected_signal":"s","priority":"low"}"#,
        )
        .unwrap();
        assert_eq!(lead.lead_type, "search");
        assert!(lead.evidence_ids.is_empty());
        let viewer: ViewerPerceptionSubmission =
            serde_json::from_str(r#"{"viewer_id":"v","profile_summary":"x"}"#).unwrap();
        assert!(viewer.leads.is_empty());
        let audience: AudienceSituationSubmission =
            serde_json::from_str(r#"{"executive_summary":"x"}"#).unwrap();
        assert!(audience.leads.is_empty());
    }
}
