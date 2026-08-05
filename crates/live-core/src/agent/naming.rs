//! 复盘卡 AI 命名（迭代细则 v1 §1 P0-2）：AI 只干一件事——给规则层算出的
//! 密度峰与复读句做语义命名，外加一句「明天复用/弃用」和切片切口建议。
//!
//! 分工纪律（程序事实 / AI 语义，AGENTS §2.1）：
//! - 输入只含 recap.rs 已算好的四个数与原文句子（grounded，零自由检索）；
//! - 输出四段短文，经终局 Tool Call 提交并做长度/非空校验；
//! - 无峰且无复读句 → 调用方跳过本 Agent（没有可命名对象，不为凑卡强拉模型）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::runtime::{
    AgentRuntime, AgentRuntimeError, AgentSpec, AgentTool, AttemptPlan, RunCtx, SubmissionSlot,
    TerminalOutcome, Trace, make_terminal_tool, run_toolcall_agent,
};
use crate::episodes::now_iso;
use crate::recap::{RecapCard, RecapNaming};

/// 场景名/现象名上限（字符）。依据：卡片一格的呼吸位；超出即流水账，
/// 12 字足够「穿纸之吻」式命名。细则未钉死数值——命名留档（AGENTS §4 禁魔数）。
pub const NAMING_NAME_MAX_CHARS: usize = 12;
/// 「明天复用/弃用」一句上限（字符）。依据：一个动作一行说完。
pub const NAMING_REUSE_MAX_CHARS: usize = 80;
/// 切片切口建议上限（字符）。依据：锚点公式（峰 ±2min）+ 一句话理由。
pub const NAMING_CUT_MAX_CHARS: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecapNamingDraft {
    pub peak_name: String,
    pub sentence_name: String,
    pub reuse_line: String,
    pub cut_advice: String,
}

pub struct RecapNamingContext {
    pub submission: Option<RecapNamingDraft>,
    pub slot: SubmissionSlot,
}

impl RunCtx for RecapNamingContext {
    fn slot(&mut self) -> &mut SubmissionSlot {
        &mut self.slot
    }
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn validate_draft(card: &RecapCard, draft: &RecapNamingDraft) -> Vec<String> {
    let mut errors = Vec::new();
    let check =
        |label: &str, value: &str, allow_empty: bool, cap: usize, errors: &mut Vec<String>| {
            let value = value.trim();
            if value.is_empty() && !allow_empty {
                errors.push(format!("{label} 不得为空"));
            }
            if char_len(value) > cap {
                errors.push(format!("{label} 超长：{} > {cap} 字", char_len(value)));
            }
        };
    check(
        "peak_name",
        &draft.peak_name,
        card.peak.is_none(),
        NAMING_NAME_MAX_CHARS,
        &mut errors,
    );
    check(
        "sentence_name",
        &draft.sentence_name,
        card.repeated.is_none(),
        NAMING_NAME_MAX_CHARS,
        &mut errors,
    );
    check(
        "reuse_line",
        &draft.reuse_line,
        false,
        NAMING_REUSE_MAX_CHARS,
        &mut errors,
    );
    check(
        "cut_advice",
        &draft.cut_advice,
        false,
        NAMING_CUT_MAX_CHARS,
        &mut errors,
    );
    errors
}

pub fn recap_naming_tools(card: RecapCard) -> Vec<AgentTool<RecapNamingContext>> {
    vec![make_terminal_tool(
        "submit_recap_naming",
        "提交四段命名（峰名/现象名/复用一句/切片切口）；这是唯一有效终局。",
        move |ctx: &mut RecapNamingContext, draft: &RecapNamingDraft| {
            let errors = validate_draft(&card, draft);
            if errors.is_empty() {
                ctx.submission = Some(draft.clone());
                TerminalOutcome::Accept(json!({"accepted": true}))
            } else {
                TerminalOutcome::Reject(errors)
            }
        },
    )]
}

/// 命名 Agent 的指令：只命名，不复述数字，不编造输入之外的事件。
fn recap_instructions() -> String {
    "你是下播复盘的命名助手。输入是程序已核算好的四个数与原文句子；\
     你的任务是且仅是：\
     1) peak_name：给密度峰起 ≤12 字的场景名（像是写在手账边上的那种命名）；\
     2) sentence_name：给复读句起 ≤12 字的现象名；\
     3) reuse_line：一句「明天复用/弃用」的判断（≤80字，只给一个方向）；\
     4) cut_advice：切片切口建议（以峰时间 ±2 分钟为锚，≤120字）。\
     纪律：只许基于输入事实；禁止编造输入中没有的事件；禁止复述数字本身；\
     输入标注「无峰/无复读句」的对应字段填\"无\"。普通文本不是有效输出，\
     必须用 submit_recap_naming 提交。"
        .to_string()
}

pub fn recap_naming_spec(card: RecapCard) -> AgentSpec<RecapNamingContext> {
    AgentSpec {
        name: "Recap Naming".to_string(),
        instructions: recap_instructions(),
        tools: recap_naming_tools(card),
    }
}

/// 命名输入（grounded）：四个数 + 原文句子。不写进任何未核算事实。
fn naming_prompt(card: &RecapCard) -> String {
    let mut lines = vec![format!("一句话结论（程序直译）：{}", card.headline)];
    lines.push(format!("本场发言人数：{}", card.speakers));
    if let Some(ret) = &card.returning {
        lines.push(format!(
            "回来过的：{}/{}（前 {} 场见过）",
            ret.count, ret.base, ret.sessions_back
        ));
    }
    match &card.peak {
        Some(peak) => lines.push(format!(
            "密度峰：{} 起 {} 分钟 {} 行弹幕",
            peak.start, peak.window_minutes, peak.count
        )),
        None => lines.push("密度峰：无（本场弹幕缺时间戳）。对应 peak_name 填\"无\"。".to_string()),
    }
    match &card.repeated {
        Some(rep) => lines.push(format!("复读句：「{}」× {}", rep.text, rep.count)),
        None => {
            lines.push("复读句：无（本场没有达标复读）。对应 sentence_name 填\"无\"。".to_string())
        }
    }
    if !card.unknown.is_empty() {
        lines.push(format!("已标明的未知：{}", card.unknown.join("；")));
    }
    lines.join("\n")
}

/// 跑一轮命名终局。Err = 协议/网络/拒绝问题——调用方响铃并把 naming 留空。
pub async fn run_recap_naming(
    runtime: &AgentRuntime,
    config: &crate::config::Config,
    card: &RecapCard,
) -> Result<RecapNaming, AgentRuntimeError> {
    let mut spec = recap_naming_spec(card.clone());
    let mut ctx = RecapNamingContext {
        submission: None,
        slot: SubmissionSlot::default(),
    };
    let trace_path = config
        .ai
        .agent
        .local_trace
        .then(|| config.output_dir.join("ai/traces/recap-naming.jsonl"));
    let mut trace = Trace::new(trace_path);
    let outcome = run_toolcall_agent::<RecapNamingContext, RecapNamingDraft>(
        runtime,
        &mut spec,
        AttemptPlan {
            label: "recap-naming",
            prompt: &naming_prompt(card),
            // 命名是一击即终局的活：4 轮上限足够「读数→提交」，不放开调查轮。
            max_turns: 4,
            retries: config.ai.agent.run_retries.max(0) as usize,
            backoff_seconds: config.ai.agent.retry_backoff_seconds,
            token_budget: None,
        },
        &mut ctx,
        &mut trace,
    )
    .await?;
    let draft = outcome.submission;
    Ok(RecapNaming {
        peak_name: draft.peak_name.trim().to_string(),
        sentence_name: draft.sentence_name.trim().to_string(),
        reuse_line: draft.reuse_line.trim().to_string(),
        cut_advice: draft.cut_advice.trim().to_string(),
        named_at: now_iso(),
    })
}
