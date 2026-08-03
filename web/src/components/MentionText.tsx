/**
 * 签名组件：在 Episode 字段原文上叠 mention 高亮。
 *
 * 区间语义与 live-core `validate_span` 同齿：start/end 是 Unicode 码点
 * （code point）下标，左闭右开——JS 必须按 Array.from 的码点切片，绝不能
 * 走 string.slice 的 UTF-16 码元（CJK 大多巧合一致，emoji/扩展平面会错标）。
 *
 * 防御约定：越界/倒置/零长/与已渲染区间重叠的 mention 一律静默跳过（后端
 * 入库已过 validate_span，这里仅对「展示期数据演化」降级，不重报）。
 */
import type { ReactNode } from "react";

export interface MentionSpanLike {
  mention_id?: string;
  text?: string;
  start_offset?: number;
  end_offset?: number;
  mention_type?: string;
  canonical_name?: string | null;
}

interface Segment {
  start: number;
  end: number;
  span: MentionSpanLike;
}

function normalize(text: string, spans: MentionSpanLike[]): Segment[] {
  const points = Array.from(text);
  const candidates = spans
    .filter(
      (span): span is MentionSpanLike & { start_offset: number; end_offset: number } =>
        typeof span.start_offset === "number" &&
        typeof span.end_offset === "number" &&
        span.start_offset >= 0 &&
        span.end_offset > span.start_offset &&
        span.end_offset <= points.length,
    )
    .sort((a, b) => a.start_offset - b.start_offset || b.end_offset - a.end_offset);
  const picked: Segment[] = [];
  let cursor = 0;
  for (const span of candidates) {
    if (span.start_offset < cursor) continue; // 与已选区间重叠 → 后到的让位
    picked.push({ start: span.start_offset, end: span.end_offset, span });
    cursor = span.end_offset;
  }
  return picked;
}

export function MentionText(props: { text: string; spans: MentionSpanLike[] }) {
  const { text, spans } = props;
  const points = Array.from(text);
  const segments = normalize(text, spans);
  if (segments.length === 0) {
    return <span>{text}</span>;
  }
  const parts: ReactNode[] = [];
  let cursor = 0;
  segments.forEach((segment, index) => {
    if (segment.start > cursor) {
      parts.push(points.slice(cursor, segment.start).join(""));
    }
    const marked = points.slice(segment.start, segment.end).join("");
    const title = [
      segment.span.mention_type,
      segment.span.canonical_name ? `→ ${segment.span.canonical_name}` : null,
    ]
      .filter(Boolean)
      .join(" ");
    parts.push(
      <mark key={segment.span.mention_id ?? index} title={title || undefined}>
        {marked}
      </mark>,
    );
    cursor = segment.end;
  });
  if (cursor < points.length) {
    parts.push(points.slice(cursor).join(""));
  }
  return <span>{parts}</span>;
}
