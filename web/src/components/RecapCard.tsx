/**
 * 复盘卡（迭代细则 v1 §1 P0-2）：战友递的一张纸——
 * 一句话结论（规则 headline）+ 三个证据（发言人数/回来过、密度峰、复读句）
 * + 一个动作（AI 命名件 reuse_line）+ 「未知的部分」一行（恒在，验收钉④）。
 *
 * 分层纪律：四个数是程序事实（pipeline 预核算，零本地再算）；命名件是 AI 语义——
 * naming=null 时动作位具名呈现「未命名」，绝不补文案（AGENTS：没有现成答案）。
 * 空场不是错误：status=empty 呈现诚实文案（Hamilton：0 同接也有价值）。
 */
import { fmtInt, fmtTime } from "../format";

export interface RecapPayload {
  status: "ready" | "empty";
  generated_at?: string | null;
  session?: { start?: string; end?: string; rid?: string } | null;
  headline?: string | null;
  speakers?: number | null;
  returning?: { count: number; base: number; sessions_back: number } | null;
  peak?: { start?: string; count: number; window_minutes: number } | null;
  repeated?: { text: string; count: number } | null;
  naming?: {
    peak_name: string;
    sentence_name: string;
    reuse_line: string;
    cut_advice: string;
    named_at?: string;
  } | null;
  unknown?: string[];
  empty_copy?: string | null;
}

export function RecapCard({ recap }: { recap: RecapPayload | null | undefined }) {
  // 钉④：「未知的部分」行恒存在——三态（未生成/空场/就绪）各自成行，不出缺位格。
  if (recap == null) {
    return (
      <section className="section card" data-testid="recap-card">
        <h2>下播复盘卡</h2>
        <div className="empty">复盘尚未生成——跑一次全量感知后，这里会有一张纸递到你手上。</div>
        <div className="muted small" data-testid="recap-unknown">
          未知的部分：复盘卡工件（ai/recap.json）还不存在。
        </div>
      </section>
    );
  }
  if (recap.status !== "ready") {
    return (
      <section className="section card" data-testid="recap-card">
        <h2>下播复盘卡</h2>
        <div className="empty" data-testid="recap-empty-copy">
          {recap.empty_copy ?? recap.headline ?? "今晚没有可复盘的话。"}
        </div>
        <div className="muted small" data-testid="recap-unknown">
          未知的部分：{(recap.unknown ?? []).length > 0 ? (recap.unknown ?? []).join("；") : "暂无。"}
        </div>
      </section>
    );
  }

  const naming = recap.naming ?? null;
  return (
    <section className="section card" data-testid="recap-card">
      <div className="section-title">
        <h2>下播复盘卡</h2>
        <span className="muted small">
          {recap.session?.start ? `${fmtTime(recap.session.start)} 这一场` : "最新一场"}
        </span>
      </div>
      {/* 一句话结论（规则直译） */}
      <p data-testid="recap-headline">{recap.headline}</p>

      {/* 三个证据（程序事实，各自可溯） */}
      <div className="grid stats static" data-testid="recap-evidence">
        <div className="card stat">
          <strong>{fmtInt(recap.speakers ?? null)}</strong>
          <span>
            发言人数
            {recap.returning
              ? `（${recap.returning.count}/${recap.returning.base} 回来过，前 ${recap.returning.sessions_back} 场）`
              : "（首场没有回来可算）"}
          </span>
        </div>
        <div className="card stat">
          <strong>{recap.peak ? `${fmtInt(recap.peak.count)} 行` : "—"}</strong>
          <span>
            {recap.peak
              ? `${recap.peak.window_minutes} 分钟最密 @ ${fmtTime(recap.peak.start)}` +
                (naming?.peak_name && naming.peak_name !== "无" ? `「${naming.peak_name}」` : "")
              : "密度峰（本场弹幕缺时间戳）"}
          </span>
        </div>
        <div className="card stat">
          <strong>{recap.repeated ? `「${recap.repeated.text}」` : "—"}</strong>
          <span>
            {recap.repeated
              ? `复读 ×${recap.repeated.count}` +
                (naming?.sentence_name && naming.sentence_name !== "无"
                  ? `·「${naming.sentence_name}」`
                  : "")
              : "被复读的句子（本场没有达标复读）"}
          </span>
        </div>
      </div>

      {/* 一个动作（AI 命名件；缺位具名，不补文案） */}
      <div data-testid="recap-action">
        {naming ? (
          <>
            <strong>{naming.reuse_line}</strong>
            <div className="muted small">切片切口：{naming.cut_advice}</div>
          </>
        ) : (
          <span className="muted small">动作建议待 AI 命名——下轮感知后补位。</span>
        )}
      </div>

      {/* 钉④：未知的部分（行恒存在） */}
      <div className="muted small" data-testid="recap-unknown">
        未知的部分：{(recap.unknown ?? []).length > 0 ? (recap.unknown ?? []).join("；") : "暂无。"}
      </div>
    </section>
  );
}
