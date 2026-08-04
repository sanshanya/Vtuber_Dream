/**
 * 主播卡（Z2 主页签名件）：头像 + 名字 + 签名 + 平台事实徽标（Lv/粉丝/关注/UP认证）+
 * B站空间/直播间外链。参照 laplace.live「明前奶绿」卡的信息层级——产品的第一屏
 * 必须是「人」，不是 tokens。
 *
 * 数据面 = overview.streamer（streamer.json 的 profile 段原样透传）。profile 缺档/半缺
 * 时逐字段空态降级，绝不臆造（demo 布景无 profile → 引导文案 + 双外链仍在）。
 */
import { fmtInt } from "../format";
import { NoReferrerImg } from "./NoReferrerImg";

export interface StreamerProfile {
  uid?: unknown;
  name?: unknown;
  face?: unknown;
  sign?: unknown;
  level?: unknown;
  official_title?: unknown;
  following?: unknown;
  followers?: unknown;
  profile_url?: unknown;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

export function StreamerCard({
  profile,
  streamerUid,
  roomId,
}: {
  profile: StreamerProfile | null;
  streamerUid: string;
  roomId: string;
}) {
  const name = text(profile?.name) || "主播资料未采集";
  const face = text(profile?.face);
  const sign = text(profile?.sign);
  const official = text(profile?.official_title);
  const level = typeof profile?.level === "number" ? profile.level : null;
  const followers = typeof profile?.followers === "number" ? profile.followers : null;
  const following = typeof profile?.following === "number" ? profile.following : null;
  const spaceUrl = text(profile?.profile_url) || `https://space.bilibili.com/${streamerUid}`;
  return (
    <section className="section card streamer-card" data-testid="streamer-card">
      {face ? (
        // 防盗链理由源头注在 Avatar.tsx——主播卡用 NoReferrerImg 直封（110px 自定义档，
        // 非 Avatar 三档家谱）。alt 直陈是谁的头像（主播名即意义本体，非陪衬装饰）。
        <NoReferrerImg src={face} alt={`${name} 头像`} className="streamer-face" />
      ) : (
        // FE-F2/R3#9：aria-label 挂在通用 div 上须补 role（img=图片语义），否则读屏丢弃标签。
        <div className="streamer-face streamer-face-fallback" role="img" aria-label="无头像">
          {name.slice(0, 1) || "?"}
        </div>
      )}
      <div className="streamer-body">
        <h2>{name}</h2>
        {profile !== null ? (
          <>
            <div className="badges">
              <span className="badge fact">UID {text(profile.uid) || streamerUid}</span>
              {level !== null && <span className="badge fact">Lv{level}</span>}
              {followers !== null && <span className="badge fact">粉丝 {fmtInt(followers)}</span>}
              {following !== null && <span className="badge fact">关注 {fmtInt(following)}</span>}
              {official && <span className="badge action">{official}</span>}
            </div>
            {sign && <p className="streamer-sign">{sign}</p>}
          </>
        ) : (
          <p className="muted">
            streamer.json 尚无主播资料——用页面顶部页头的「触发全量感知」跑一轮后呈现头像与签名。
          </p>
        )}
        <p className="streamer-links">
          <a href={spaceUrl} target="_blank" rel="noreferrer">
            B站空间
          </a>
          <a href={`https://live.bilibili.com/${roomId}`} target="_blank" rel="noreferrer">
            直播间 {roomId}
          </a>
        </p>
      </div>
    </section>
  );
}
