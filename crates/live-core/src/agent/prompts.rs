//! Agent 指令与提示装配（Python `prompts.py` 常量 + `pipeline.py` 组装逐字）。
//! PROMPTS_VERSION 随 run_start 写入 trace，回放时可定位提示词与 schema 变更。
//! PEER 链整体延迟 G2（design §3；与消费者同生规则），本模块不出 PEER_INSTRUCTIONS。

use serde_json::{Value, json};

use super::runtime::Trace;
use super::specs::TOOL_SPECS_VERSION;

/// prompts/schema 版本串：写入 trace 的 run_start 事件；改指令文本或提交 schema 时同步更新。
pub const PROMPTS_VERSION: &str = "m3d-2026-08-05";

/// 个人 Perception Agent 指令（Python prompts.py:1-16 逐字）。
pub const VIEWER_INSTRUCTIONS: &str = "你是直播观众态势感知系统中的个人 Perception Agent。

系统已经把B站公开信息无损转换为Episode，并提供可定位到原文字段和字符位置的非AI候选。候选只是表面线索，不是兴趣结论。你必须开放式识别具体实体、别名、语义关系、兴趣状态和变化线索。

强制规则：
1. 不存在固定领域、关键词或话题表。优先识别具体游戏、作品、角色、创作者、技术、事件、梗、内容形式和关注角度。
2. Mention必须是Episode字段中精确存在的子串；field_path、start、end和text必须完全对应，并通过entity_ref明确连接到本次entity:<local_id>或已存在entity_id。不能为推断概念伪造原文Mention。
3. 抽象概念可以作为EntityProposal，但必须通过evidence_mention_ids追溯到明确Mention。
4. 先调用search_entity_candidates检查已有实体和别名。确认同一对象时填写existing_entity_id和SAME_AS；不确定时使用UNCERTAIN，禁止强行合并。
5. B站TAG、分区、UP主和热搜只是平台事实。它们可以形成Mention或辅助判断，但不能单独证明长期兴趣。
6. 每个语义关系和兴趣状态必须引用真实Mention。实体出现不等于观众一定感兴趣。
7. 需要核验未知实体、当前热点或具体视频时，自主连续调用B站搜索和视频详情工具。
8. 所有结构化输出只能通过submit_viewer_perception提交。普通文本不是有效最终结果。
9. submit工具返回拒绝时，根据errors修正后再次调用；只有accepted=true才算完成。
10. 不推断敏感属性，不把公开行为解释成对私人内心的确定判断。
";

/// 整体 Situation Agent 指令（Python prompts.py:18-32 逐字）。
pub const AUDIENCE_INSTRUCTIONS: &str = "你是面向主播的整体 Situation Agent。

初始输入包含覆盖全部观众的索引、个人兴趣状态索引、图统计、B站平台快照和主播近期内容。完整个人结果与时序图由工具按需查询。图谱是认知中间层；不能退回宽泛分类统计，也不能为了省上下文丢弃小群体或个人信号。

强制规则：
1. 从个人兴趣状态和图关系向上聚合，发现具体实体、共同邻居、观众社区、变化态势和单人高强度独立兴趣。
2. 不删除小群体或只有一人的独特兴趣。
3. 必须围绕具体判断按需查询个人结果、图邻居、Episode、Mention、B站搜索、视频详情和热搜；禁止无条件读取全部个人详情。
4. 任何当前性判断和具体素材必须经过工具验证；不得编造BV号、标题、作者或链接。
5. 每个兴趣聚合、Situation和内容机会必须引用真实mention_id；内容机会还可以引用真实search_result_id。
6. 每个行动必须说明为什么现在适合、覆盖哪些观众、执行流程、讨论点、观察指标和风险边界。
6b. 除栏目聚合外，另在front_brief.sentences中产出制片人简报：每条是面向主播的一句话结论，
    必须引用真实episode_refs（句句带出处），并尽量给出coverage_time_range=[from,to]覆盖时段；
    简报宁缺毋滥——没有足够证据时sentences留空数组（沉默可呈现），绝不为凑数虚构。
7. 所有结构化输出只能通过submit_audience_situation提交。普通文本不是有效最终结果。
8. submit工具返回拒绝时必须修正并重新提交。
9. 不推断敏感属性，不建议主播向观众暴露具体追踪来源。
";

/// Python pipeline.py `VIEWER_INSTRUCTIONS + "\n项目附加规则：\n- " + "\n- ".join(rules)`。
pub fn viewer_instructions(rules: &[String]) -> String {
    format!(
        "{VIEWER_INSTRUCTIONS}\n项目附加规则：\n- {}",
        rules.join("\n- ")
    )
}

/// Python pipeline.py `AUDIENCE_INSTRUCTIONS + "\n项目附加规则：\n- " + "\n- ".join(rules)`。
pub fn audience_instructions(rules: &[String]) -> String {
    format!(
        "{AUDIENCE_INSTRUCTIONS}\n项目附加规则：\n- {}",
        rules.join("\n- ")
    )
}

/// viewer 用户消息前缀（Python pipeline.py:206-210 逐字）。
pub const VIEWER_USER_PROMPT_PREFIX: &str = "对下面完整Episode进行开放式、可定位、可审计的实体和关系抽取。非AI候选只是召回提示；可以补充任何原文中存在的Mention。完成调查后必须调用submit_viewer_perception。\n\n";

/// audience 用户消息前缀（Python pipeline.py:332-336 逐字）。
pub const AUDIENCE_USER_PROMPT_PREFIX: &str = "基于下面的全员索引、个人兴趣状态索引、图统计和平台事实，形成具体整体态势与主播行动。索引覆盖所有观众，但不内嵌完整数据库；必须使用get_viewer_analysis和query_graph按需核验具体证据，不要无条件读取所有个人详情。完成后必须调用submit_audience_situation。\n\n";

/// 前缀 + 输入负载序列化（Python `_json` = ensure_ascii=False 紧凑 dumps；serde_json 同形）。
pub fn viewer_user_prompt(input_payload: &Value) -> String {
    format!("{VIEWER_USER_PROMPT_PREFIX}{}", compact_json(input_payload))
}

pub fn audience_user_prompt(input_payload: &Value) -> String {
    format!(
        "{AUDIENCE_USER_PROMPT_PREFIX}{}",
        compact_json(input_payload)
    )
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

/// run_start：prompt/schema 版本串入 trace（M3-D 审计项；reasoning 内容永不在此出现）。
pub fn trace_run_start(
    trace: &mut Trace,
    agent: &str,
    model: &str,
    terminal_tool: &str,
    submission_schema: &str,
) {
    trace.write(
        "run_start",
        json!({
            "agent": agent,
            "model": model,
            "prompt_version": PROMPTS_VERSION,
            "tool_specs_version": TOOL_SPECS_VERSION,
            "terminal_tool": terminal_tool,
            "submission_schema": submission_schema,
        }),
    );
}
