/**
 * no-referrer 图片基件：防盗链纪律的物理封装（理由源头注在 Avatar.tsx，全站只此一份）。
 * 新消费面禁止裸 <img> 直链 hdslb——一律走本组件（或其上的 Avatar）。
 */
export function NoReferrerImg({
  src,
  alt,
  className,
}: {
  src: string;
  alt: string;
  className?: string;
}) {
  return (
    <img src={src} alt={alt} className={className} referrerPolicy="no-referrer" loading="lazy" />
  );
}
