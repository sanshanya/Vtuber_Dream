/**
 * 时效位徽标「信源已更新·待重判」——三处消费面（舰长列表 / 个人树感知区 /
 * 首页舰长 strip）的文案与熄灭路径 title 单一来源。className 复用 .badge.action
 * （不另造新类）；data-testid 按消费面区分，断言一律 getByTestId 锚点
 * （教训：文案与 note 可能同语撞车，绝不用文本全文匹配）。
 * 可达性：熄灭路径不止藏 title（hover 不可达的面也有知情权）——随行
 * .muted small 直陈，title 保留完整因果。
 */
export function AiStaleBadge({ testId }: { testId: string }) {
  return (
    <>
      <span
        className="badge action"
        data-testid={testId}
        title="该舰长的事实面（采集内容）已有更新，现有 AI 结论基于旧信源——重跑「舰长 AI 分析」后熄灭"
      >
        信源已更新·待重判
      </span>
      <span className="muted small">重跑「舰长 AI 分析」后熄灭</span>
    </>
  );
}
