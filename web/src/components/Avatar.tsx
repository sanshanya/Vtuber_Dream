/**
 * 头像统一面：face 有 → NoReferrerImg（防盗链）；无 → 首字 fallback 块
 * （role="img" + aria-label，占位是谁直陈，不是装饰性灰盘）。
 *
 * 防盗链源头注（全站只此一份，其余消费点引用即可）：B站 hdslb 图床校验 Referer——
 * 浏览器默认携带本站 Referer 请求头像/封面会被 403 拒（裂图/白图），必须
 * referrerPolicy="no-referrer"；NoReferrerImg 即此封装。
 *
 * 尺寸三档 = styles.css 三族类：md=58px（默认卡头）/ sm=34px（表格行）/ xs=26px（strip chip）。
 */
import { NoReferrerImg } from "./NoReferrerImg";

export type AvatarSize = "xs" | "sm" | "md";

const SIZE_CLASS: Record<AvatarSize, string> = {
  xs: "avatar-xs",
  sm: "avatar-sm",
  md: "",
};

export function Avatar({
  face,
  name,
  size = "md",
}: {
  face: string | null | undefined;
  name: string | null | undefined;
  size?: AvatarSize;
}) {
  const className = ["avatar", SIZE_CLASS[size]].filter(Boolean).join(" ");
  if (face) {
    // 名字随旁边的文本节点同行呈现 → 头像对读屏是装饰，alt 置空。
    return <NoReferrerImg src={face} alt="" className={className} />;
  }
  return (
    <span
      className={`${className} avatar-fallback`}
      role="img"
      aria-label={name ? `${name} 头像` : "无头像"}
    >
      {(name ?? "").slice(0, 1) || "?"}
    </span>
  );
}
