/**
 * 观众列表空池单查引导位（§12 冷启动 / M5-C 签名件）。
 * EmptyPoolHint 是纯组件：uid 串与提交接线由 props 入——vitest 不摸 fetch。
 */
export function EmptyPoolHint(props: {
  uid: string;
  pending: boolean;
  /** 单飞互斥契约：有在飞 run 时禁触发钮（文本框照旧可输入）。 */
  runActive: boolean;
  onUidChange: (uid: string) => void;
  onSubmit: () => void;
}) {
  return (
    <div className="notice" data-testid="empty-pool-hint">
      <h3>观众池为空</h3>
      <p>
        全量感知以大航海名单为入口；名单为空时可以<strong>单查指定观众</strong>
        作为冷启动种子（seed_source=manual）。
      </p>
      <div className="toolbar">
        <input
          aria-label="单查观众 uid"
          placeholder="B站观众 uid（如 1003 或 demo-1）"
          value={props.uid}
          onChange={(event) => props.onUidChange(event.target.value)}
        />
        <button
          disabled={props.pending || props.uid.trim() === "" || props.runActive}
          onClick={props.onSubmit}
        >
          {props.pending ? "已提交…" : "单查该观众"}
        </button>
      </div>
    </div>
  );
}
