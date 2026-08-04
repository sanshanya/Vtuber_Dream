/**
 * 极简 Markdown 渲染（Z2：audience 提交的 executive_summary 原本被整个塞进
 * 单个 <p>，## / ** 记号大字报式漏上墙——M6-C② 截图现场）。渲染面只承诺
 * 四级语法子集：#~##### 标题、- 无序列表、**粗体**、`行内码`，其余按段落直出。
 *
 * 纯 React 节点拼装，不走 innerHTML——AI 产物文本零 HTML 注入面；不引
 * react-markdown（M5 依赖面收敛纪律，我们只缺这四级语法）。
 */
import type { ReactNode } from "react";

/** 行内：**bold** / `code` 单趟切分；其余文本片段原样上屏。 */
function inline(text: string, keyPrefix: string): ReactNode[] {
  const parts: ReactNode[] = [];
  const re = /\*\*([^*]+)\*\*|`([^`]+)`/g;
  let last = 0;
  let n = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    if (m[1] !== undefined) {
      parts.push(<strong key={`${keyPrefix}-b${n}`}>{m[1]}</strong>);
    } else {
      parts.push(<code key={`${keyPrefix}-c${n}`}>{m[2]}</code>);
    }
    last = m.index + m[0].length;
    n += 1;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

export function Markdown({ text }: { text: string }) {
  const lines = text.split(/\r?\n/);
  const nodes: ReactNode[] = [];
  let list: string[] = [];
  let para: string[] = [];
  let block = 0;

  const flushList = () => {
    if (list.length === 0) return;
    const key = `u${block}`;
    nodes.push(
      <ul key={key}>
        {list.map((item, i) => (
          <li key={i}>{inline(item, `${key}-${i}`)}</li>
        ))}
      </ul>,
    );
    list = [];
    block += 1;
  };
  const flushPara = () => {
    if (para.length === 0) return;
    const key = `p${block}`;
    // AI 常在段落内随手换行；聚成一段（行内记号照常解析）。
    nodes.push(<p key={key}>{inline(para.join(" "), key)}</p>);
    para = [];
    block += 1;
  };

  for (const raw of lines) {
    const line = raw.trim();
    if (line.length === 0) {
      flushPara();
      flushList();
      continue;
    }
    const heading = /^(#{1,5})\s+(.*)$/.exec(line);
    if (heading) {
      flushPara();
      flushList();
      const key = `h${block}`;
      // ####/##### 统一收进 h4：卡片内不需要更深标题层级。
      const body = inline(heading[2], key);
      if (heading[1].length <= 3) nodes.push(<h3 key={key}>{body}</h3>);
      else nodes.push(<h4 key={key}>{body}</h4>);
      block += 1;
      continue;
    }
    const bullet = /^[-*]\s+(.*)$/.exec(line);
    if (bullet) {
      flushPara();
      list.push(bullet[1]);
      continue;
    }
    flushList();
    para.push(line);
  }
  flushPara();
  flushList();
  return <div className="markdown">{nodes}</div>;
}
