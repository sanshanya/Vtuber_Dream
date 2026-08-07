//! 历史管理：回放窗口化 + 中间轮折叠 + token 估算。
//!
//! 职责：只动 LLM 工作记忆，不动任何事实/证据层（AGENTS.md §7——事实入 Episode/图存）。
//! 与 runtime 的分工：runtime 管协议契约（终局/重试/trace），本模块管「历史多长就算多、
//! 多长就该压」——一段独立撰器。

use serde_json::json;

use super::runtime::{CHARS_PER_TOKEN, OaiMessage, Trace, truncate_chars};

/// 中间轮折叠参数。所有阈值有命名+默认+测试（AGENTS.md §4）：
/// - trigger_tokens：估算 prompt tokens（字节秤/4）超线触发；典型 200_000（≈dsv4 500K 窗的 40%）；
/// - keep_tail_turns：折叠后保留末尾完整轮数；
/// - entry_chars：折叠摘要单轮条目的字符预算。
#[derive(Debug, Clone)]
pub struct FoldConfig {
    pub trigger_tokens: u32,
    pub keep_tail_turns: usize,
    pub entry_chars: usize,
}

/// reasoning 回放窗口化——只压 LLM 工作记忆。
///
/// 语义：末 k 条「带 tool_calls 的 assistant」保留原文；更早的同样消息保留
/// reasoning_content 字段但内容置空串（不剥字段——空串在 dsv4 执法矩阵下
/// 同样豁免 400，见 docs/2026-08-05-pi-source-verdict.md §6）。
/// replay_reasoning=false 时 push 落历史已剥成 None，此处自然无物可窗。
pub fn apply_replay_window(
    replay_window: Option<u32>,
    mut messages: Vec<OaiMessage>,
) -> Vec<OaiMessage> {
    let Some(k) = replay_window else {
        return messages;
    };
    let idxs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()))
        .map(|(i, _)| i)
        .collect();
    let keep_from = idxs.len().saturating_sub(k as usize);
    for (pos, &i) in idxs.iter().enumerate() {
        if pos < keep_from
            && let Some(rc) = &messages[i].reasoning_content
            && !rc.is_empty()
        {
            messages[i].reasoning_content = Some(String::new());
        }
    }
    messages
}

/// 历史 token 粗估 = 字节秤 / CHARS_PER_TOKEN。中文须知在 DeepSeek
/// 上是每字多 byte——本估算只负责「量级触发」，不用作计费/预算账本
/// （预算已有 viewer_token_budget 按 server usage 实记）。
pub fn estimate_tokens(messages: &[OaiMessage]) -> u64 {
    let mut bytes: usize = 0;
    for m in messages {
        if let Some(c) = &m.content {
            bytes += c.len();
        }
        if let Some(rc) = &m.reasoning_content {
            bytes += rc.len();
        }
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                bytes += c.id.len() + c.function.name.len() + c.function.arguments.len();
            }
        }
        if let Some(id) = &m.tool_call_id {
            bytes += id.len();
        }
    }
    bytes.div_ceil(CHARS_PER_TOKEN) as u64
}

/// 折叠。
/// - 触发：估 tokens > trigger；不动事实层：被折叠的全部是 assistant/tool 工作记忆，
///   证据在 Episode/图存（AGENTS.md §7）。
/// - 结构：保持「assistant 与 tool 对」的整体搬迁——折叠产生邻接保持的不变量；
///   被折叠的每一轮在摘要里留工具名+参数截段（程序生成，非 AI 归纳）。
/// - 保留：头（system + 首个 user prompt）+ 末 keep_tail_turns 个完整轮。
pub fn maybe_fold(
    fold: Option<&FoldConfig>,
    agent_name: &str,
    history: &mut Vec<OaiMessage>,
    trace: &mut Trace,
) {
    let Some(fold) = fold else {
        return;
    };
    let est_before = estimate_tokens(history);
    if est_before <= fold.trigger_tokens as u64 {
        return;
    }

    // 找第一个 assistant 起始位置（head 保留区）。
    let Some(first_assistant) = history.iter().position(|m| m.role == "assistant") else {
        return;
    };
    // 分轮：每条 assistant 开一个轮，其后连续 role==tool 的消息归属于该轮。
    let turn_starts: Vec<usize> = history
        .iter()
        .enumerate()
        .skip(first_assistant)
        .filter(|(_, m)| m.role == "assistant")
        .map(|(i, _)| i)
        .collect();
    if turn_starts.is_empty() {
        return;
    }
    let total_turns = turn_starts.len();
    if total_turns <= fold.keep_tail_turns {
        return; // 末区已是最短可折叠形态，无可折
    }
    // 保留末 keep_tail_turns 个轮：切除区 = [first_assistant, tail_start)
    // keep_tail_turns=0（config 尺度为 0 的合法值）=「全折到只剩头」——
    // tail_start 就是 history 尾端（turn_starts 无此下标，绝不直查）。
    let tail_start = if fold.keep_tail_turns == 0 {
        history.len()
    } else {
        turn_starts[total_turns - fold.keep_tail_turns]
    };
    let middle: Vec<OaiMessage> = history.drain(first_assistant..tail_start).collect();

    // 生成摘要：每轮 1-2 条记录，程序摘取：assistant.tool_call name + args 截段 + 对应 tool result 头段。
    let mut entries: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut turn_no = 1usize;
    while cursor < middle.len() {
        let m = &middle[cursor];
        if m.role != "assistant" {
            cursor += 1;
            continue;
        }
        if let Some(calls) = &m.tool_calls {
            for call in calls {
                let args_snip = truncate_chars(&call.function.arguments, fold.entry_chars);
                // 找该工具调用对应的 tool result（同轮或顺延，不做全联搜索）。
                let mut result_head = String::new();
                let mut j = cursor + 1;
                while j < middle.len() && middle[j].role == "tool" {
                    if middle[j].tool_call_id.as_deref() == Some(call.id.as_str()) {
                        result_head = truncate_chars(
                            middle[j].content.as_deref().unwrap_or(""),
                            fold.entry_chars,
                        );
                        break;
                    }
                    j += 1;
                }
                entries.push(format!(
                    "- 轮 {turn_no}: {}({args_snip}) → {result_head}",
                    call.function.name
                ));
            }
        } else if let Some(content) = &m.content {
            let snip = truncate_chars(content, fold.entry_chars);
            if !snip.is_empty() {
                entries.push(format!("- 轮 {turn_no}: 文本轮 {snip}"));
            }
        }
        turn_no += 1;
        // 前进过本 assistant 自身，再跳过连续的 tool 消息——
        // 先 +1 是关键：跳过循环只认 tool，assistant 不自进一次就死循环。
        cursor += 1;
        while cursor < middle.len() && middle[cursor].role == "tool" {
            cursor += 1;
        }
    }

    let header = format!(
        "[历史折叠 · P2-γ] 上轮区间 1..={total_turns} 中第 1..={} 轮已被程序折叠，\
         为控制上下文长度。如需具体证据请重新调用对应调查工具。",
        total_turns - fold.keep_tail_turns
    );
    let content = if entries.is_empty() {
        header
    } else {
        format!("{header}\n{}", entries.join("\n"))
    };
    let folded = OaiMessage::user(content);
    let removed = middle.len();
    let inserted_at = first_assistant;
    history.insert(inserted_at, folded);

    let est_after = estimate_tokens(history);
    trace.write(
        "fold_history",
        json!({
            "agent": agent_name,
            "trigger_tokens": fold.trigger_tokens,
            "est_before": est_before,
            "est_after": est_after,
            "removed_messages": removed,
        }),
    );
}
