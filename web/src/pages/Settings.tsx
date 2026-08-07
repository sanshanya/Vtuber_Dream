import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { api } from "../api";
import { errText, fmtCny } from "../format";

/**
 * 设置页：白名单五键可写（cookie/api_key/base_url/model/run_budget_cny），
 * 空串 = 保持现值；预算键回显现值（null=未设闸）兼月度实耗行（/api/budget）。
 * 其余面只读直出，永不回显原文。
 */
export function Settings() {
  const queryClient = useQueryClient();
  const config = useQuery({ queryKey: ["config"], queryFn: api.config });
  const budget = useQuery({ queryKey: ["budget"], queryFn: api.budget });
  const [form, setForm] = useState({
    cookie: "",
    api_key: "",
    base_url: "",
    model: "",
    run_budget_cny: "",
  });
  const [feedback, setFeedback] = useState<string | null>(null);
  const [isError, setIsError] = useState(false);
  const [pending, setPending] = useState(false);

  async function save() {
    setFeedback(null);
    setPending(true);
    try {
      const result = await api.putConfig({
        bilibili: { cookie: form.cookie },
        ai: {
          api_key: form.api_key,
          base_url: form.base_url,
          model: form.model,
          run_budget_cny: form.run_budget_cny,
        },
      });
      setIsError(false);
      setFeedback(result.status === "updated" ? `已写入 ${result.keys} 个键` : "无变化");
      setForm({ cookie: "", api_key: "", base_url: "", model: "", run_budget_cny: "" });
      await queryClient.invalidateQueries({ queryKey: ["config"] });
    } catch (error) {
      setIsError(true);
      setFeedback(errText(error));
    } finally {
      setPending(false);
    }
  }

  if (config.isLoading) return <div className="state-loading">载入设置…</div>;
  if (config.isError)
    return (
      <div className="notice">
        {String(config.error instanceof Error ? config.error.message : config.error)}
      </div>
    );
  const data = config.data;
  if (!data) {
    // 查询缓存层（严格模式下 status 收窄不保证 data 非空）——显式守卫。
    return <div className="state-loading">载入设置…</div>;
  }

  return (
    <section className="section">
      <div className="section-title">
        <h2>设置</h2>
      </div>
      <div className="grid two">
        <div className="card">
          <h3>可写键（白名单）</h3>
          <div className="settings-form">
            <label>
              B站 Cookie（{data.bilibili.cookie_present ? "已配置" : "未配置"}，空 = 保持）
              <input
                type="password"
                value={form.cookie}
                onChange={(e) => setForm({ ...form, cookie: e.target.value })}
                autoComplete="off"
              />
            </label>
            <label>
              AI API Key（{data.ai.api_key_present ? "已配置" : "未配置"}，空 = 保持）
              <input
                type="password"
                value={form.api_key}
                onChange={(e) => setForm({ ...form, api_key: e.target.value })}
                autoComplete="off"
              />
            </label>
            <label>
              base_url（空 = 保持）
              <input
                value={form.base_url}
                placeholder={data.ai.base_url}
                onChange={(e) => setForm({ ...form, base_url: e.target.value })}
              />
            </label>
            <label>
              model（空 = 保持）
              <input
                value={form.model}
                placeholder={data.ai.model}
                onChange={(e) => setForm({ ...form, model: e.target.value })}
              />
            </label>
            <label>
              AI 单次预算（元，预估超支即阻断；空 = 保持）
              <input
                type="text"
                inputMode="decimal"
                value={form.run_budget_cny}
                placeholder={data.ai.run_budget_cny == null ? "未设限" : String(data.ai.run_budget_cny)}
                onChange={(e) => setForm({ ...form, run_budget_cny: e.target.value })}
              />
            </label>
            <div className="toolbar">
              <button className="primary" disabled={pending} onClick={() => void save()}>
                {pending ? "写入中…" : "保存设置"}
              </button>
              {feedback && (
                <span className={`badge ${isError ? "danger" : "state"}`}>{feedback}</span>
              )}
            </div>
          </div>
        </div>
        <div className="card">
          <h3>当前配置（只读面）</h3>
          <dl className="kv">
            <dt>project</dt>
            <dd>{data.project_name}</dd>
            <dt>output_dir</dt>
            <dd className="protocol">{data.output_dir}</dd>
            <dt>room_id</dt>
            <dd>{data.bilibili.room_id}</dd>
            <dt>streamer_uid</dt>
            <dd>{data.bilibili.streamer_uid}</dd>
            <dt>base_url</dt>
            <dd className="protocol">{data.ai.base_url}</dd>
            <dt>model</dt>
            <dd className="protocol">{data.ai.model}</dd>
          </dl>
          <h3>可写键清单</h3>
          <div className="badges">
            {(data.writable_keys ?? []).map((key) => (
              <span className="badge" key={key}>
                {key}
              </span>
            ))}
          </div>
          <h3>月度实耗（history.jsonl 汇总）</h3>
          {budget.isLoading ? (
            <div className="state-loading">载入预算面…</div>
          ) : budget.isError ? (
            <div className="notice">{errText(budget.error)}</div>
          ) : budget.data ? (
            <div className="muted small" data-testid="monthly-budget">
              本月（{budget.data.month}）AI 实耗 {fmtCny(budget.data.month_cost_cny)}／
              {budget.data.month_runs} 次运行；单次预算{" "}
              {budget.data.budget_cny === null ? "未设" : fmtCny(budget.data.budget_cny)}
              {budget.data.last_run
                ? ` · 最近一次：${budget.data.last_run.kind}（${budget.data.last_run.status}）≈${fmtCny(budget.data.last_run.cost_cny)}`
                : " · 暂无历史记录"}
            </div>
          ) : null}
        </div>
      </div>
    </section>
  );
}
