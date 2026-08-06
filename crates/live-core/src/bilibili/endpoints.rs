/// `BilibiliClient` 的端点续接实现（bilibili/mod.rs 行数上限拆分）。
/// 全部方法依赖 mod.rs 的 request()/throttle()/nav()/mixin_key_sync() 等共享面。
use serde_json::Value;

use super::{
    BANGUMI_PAGE_CAP, BilibiliClient, BilibiliError, DANMAKU_SHARD_CAP, FAVORITE_ITEMS_PAGE_CAP,
    FOLLOWINGS_PAGE_CAP, HOT_SEARCHES_LIMIT_CAP, RECORD_LIST_PAGE_CAP, REPLIES_PAGE_CAP,
    SEARCH_VIDEOS_PAGE_SIZE, VIDEOS_PAGE_CAP, pick, py_int, py_truth, take_items,
};

impl BilibiliClient {
    pub fn relation_stat(&mut self, uid: &str) -> Result<Value, BilibiliError> {
        self.request(
            &self.api_base.clone(),
            "/x/relation/stat",
            &[("vmid".to_string(), Some(uid.to_string()))],
            false,
            Some(&format!("https://space.bilibili.com/{uid}")),
        )
    }

    pub fn followings(&mut self, uid: &str, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let mut rows: Vec<Value> = Vec::new();
        let mut page = 1;
        while (rows.len() as i64) < limit {
            let page_size = (limit - rows.len() as i64).min(FOLLOWINGS_PAGE_CAP);
            let data = self.request(
                &self.api_base.clone(),
                "/x/relation/followings",
                &[
                    ("vmid".to_string(), Some(uid.to_string())),
                    ("pn".to_string(), Some(page.to_string())),
                    ("ps".to_string(), Some(page_size.to_string())),
                    ("order".to_string(), Some("desc".to_string())),
                ],
                false,
                Some(&format!("https://space.bilibili.com/{uid}/fans/follow")),
            )?;
            let items: Vec<Value> = data
                .get("list")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(Value::is_object)
                .collect();
            let got = items.len() as i64;
            rows.extend(items);
            if got < page_size {
                break;
            }
            page += 1;
        }
        rows.truncate(limit as usize);
        Ok(rows)
    }

    pub fn profile(&mut self, uid: &str) -> Result<Value, BilibiliError> {
        self.request(
            &self.api_base.clone(),
            "/x/space/wbi/acc/info",
            &[("mid".to_string(), Some(uid.to_string()))],
            true,
            Some(&format!("https://space.bilibili.com/{uid}")),
        )
    }

    /// 视频列表：单页 ≤30（Python 同）；设计文档的"分页不再截断"指调用方多页思路，
    /// 此函数保持单页语义，collect 层循环翻页。
    pub fn videos(&mut self, uid: &str, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        // design 修复项："视频列表分页不再静默截断"——循环翻页直到 limit 或页不满。
        let mut rows: Vec<Value> = Vec::new();
        let mut page = 1;
        while (rows.len() as i64) < limit {
            let rows_before = rows.len() as i64;
            let page_size = (limit - rows.len() as i64).min(VIDEOS_PAGE_CAP);
            let data = self.request(
                &self.api_base.clone(),
                "/x/space/wbi/arc/search",
                &[
                    ("mid".to_string(), Some(uid.to_string())),
                    ("pn".to_string(), Some(page.to_string())),
                    ("ps".to_string(), Some(page_size.to_string())),
                    ("order".to_string(), Some("pubdate".to_string())),
                ],
                true,
                Some(&format!("https://space.bilibili.com/{uid}/video")),
            )?;
            let list = data
                .get("list")
                .and_then(|inner| inner.get("vlist"))
                .cloned()
                .unwrap_or(Value::Null);
            let items: Vec<Value> = list
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(Value::is_object)
                .collect();
            rows.extend(items);
            if rows.len() as i64 - rows_before < page_size {
                break;
            }
            page += 1;
        }
        rows.truncate(limit.max(0) as usize);
        Ok(rows)
    }

    pub fn dynamics(&mut self, uid: &str, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/polymer/web-dynamic/v1/feed/space",
            &[
                ("host_mid".to_string(), Some(uid.to_string())),
                ("offset".to_string(), Some(String::new())),
                ("timezone_offset".to_string(), Some("-480".to_string())),
            ],
            false,
            Some(&format!("https://space.bilibili.com/{uid}/dynamic")),
        )?;
        Ok(take_items(data.get("items"), limit))
    }

    pub fn favorite_folders(&mut self, uid: &str, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/v3/fav/folder/created/list-all",
            &[
                ("up_mid".to_string(), Some(uid.to_string())),
                ("type".to_string(), Some("2".to_string())),
                ("web_location".to_string(), Some("333.1387".to_string())),
            ],
            false,
            Some(&format!("https://space.bilibili.com/{uid}/favlist")),
        )?;
        // Python：[item for item in list if isinstance(dict)] if int(attr or 0) & 1 == 0
        let items: Vec<Value> = data
            .get("list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(Value::is_object)
            .collect();
        Ok(items
            .into_iter()
            .filter(|item| py_int(item.get("attr")) & 1 == 0)
            .take(limit as usize)
            .collect())
    }

    pub fn favorite_items(
        &mut self,
        media_id: &str,
        limit: i64,
    ) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let mut rows: Vec<Value> = Vec::new();
        let mut page = 1;
        while (rows.len() as i64) < limit {
            let page_size = (limit - rows.len() as i64).min(FAVORITE_ITEMS_PAGE_CAP);
            let data = self.request(
                &self.api_base.clone(),
                "/x/v3/fav/resource/list",
                &[
                    ("media_id".to_string(), Some(media_id.to_string())),
                    ("pn".to_string(), Some(page.to_string())),
                    ("ps".to_string(), Some(page_size.to_string())),
                    ("keyword".to_string(), Some(String::new())),
                    ("order".to_string(), Some("mtime".to_string())),
                    ("type".to_string(), Some("0".to_string())),
                    ("tid".to_string(), Some("0".to_string())),
                    ("platform".to_string(), Some("web".to_string())),
                ],
                false,
                Some(&format!(
                    "https://www.bilibili.com/medialist/detail/ml{media_id}"
                )),
            )?;
            // Python：先 isinstance 过滤，"页不满"用过滤后长度；has_more 按 truthiness。
            let items: Vec<Value> = data
                .get("medias")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(Value::is_object)
                .collect();
            let got = items.len() as i64;
            rows.extend(items);
            if got < page_size || !py_truth(data.get("has_more")) {
                break;
            }
            page += 1;
        }
        rows.truncate(limit as usize);
        Ok(rows)
    }

    pub fn bangumi(&mut self, uid: &str, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/space/bangumi/follow/list",
            &[
                ("vmid".to_string(), Some(uid.to_string())),
                ("type".to_string(), Some("1".to_string())),
                ("pn".to_string(), Some("1".to_string())),
                (
                    "ps".to_string(),
                    Some(limit.min(BANGUMI_PAGE_CAP).to_string()),
                ),
            ],
            false,
            Some(&format!("https://space.bilibili.com/{uid}/bangumi")),
        )?;
        Ok(take_items(data.get("list"), limit))
    }

    pub fn games(&mut self, uid: &str, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/space/lastplaygame/v2",
            &[("mid".to_string(), Some(uid.to_string()))],
            false,
            Some(&format!("https://space.bilibili.com/{uid}")),
        )?;
        Ok(take_items(data.get("list"), limit))
    }

    pub fn hot_searches(&mut self, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/web-interface/wbi/search/square",
            &[(
                "limit".to_string(),
                Some(limit.min(HOT_SEARCHES_LIMIT_CAP).to_string()),
            )],
            true,
            Some("https://www.bilibili.com/"),
        )?;
        let trending = data.get("trending").cloned().unwrap_or(Value::Null);
        Ok(take_items(trending.get("list"), limit))
    }

    /// 关键词搜索视频（wbi 签名）。E 批次删除时约定"复出时与消费者同生并带 wiremock
    /// 负例"——M3 ResearchService 出生，端点复出（Python bilibili.py:436）。
    pub fn search_videos(
        &mut self,
        keyword: &str,
        limit: i64,
        order: &str,
    ) -> Result<Vec<Value>, BilibiliError> {
        let keyword = keyword.trim();
        if keyword.is_empty() || limit <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/web-interface/wbi/search/type",
            &[
                ("search_type".to_string(), Some("video".to_string())),
                ("keyword".to_string(), Some(keyword.to_string())),
                ("order".to_string(), Some(order.to_string())),
                ("page".to_string(), Some("1".to_string())),
                (
                    "page_size".to_string(),
                    Some(limit.min(SEARCH_VIDEOS_PAGE_SIZE).to_string()),
                ),
            ],
            true,
            Some(&format!(
                "https://search.bilibili.com/all?keyword={keyword}"
            )),
        )?;
        Ok(take_items(data.get("result"), limit))
    }

    pub fn video_detail(&mut self, bvid: &str) -> Result<Value, BilibiliError> {
        if bvid.is_empty() {
            return Ok(Value::Null);
        }
        self.request(
            &self.api_base.clone(),
            "/x/web-interface/view",
            &[("bvid".to_string(), Some(bvid.to_string()))],
            false,
            Some(&format!("https://www.bilibili.com/video/{bvid}")),
        )
    }

    pub fn video_tags(&mut self, bvid: &str) -> Result<Vec<String>, BilibiliError> {
        if bvid.is_empty() {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/tag/archive/tags",
            &[("bvid".to_string(), Some(bvid.to_string()))],
            false,
            Some(&format!("https://www.bilibili.com/video/{bvid}")),
        )?;
        let mut seen: Vec<String> = Vec::new();
        if let Value::Array(items) = data {
            for item in items {
                let name = {
                    let t = pick(&item, "tag_name");
                    if t.is_empty() { pick(&item, "name") } else { t }
                };
                if !name.is_empty() && !seen.contains(&name) {
                    seen.push(name);
                }
            }
        }
        Ok(seen)
    }

    /// 评论区浅存在（design §M2-B2c 定形 2026-08-03 实测）：
    /// `/x/v2/reply` 旧式翻页无需 wbi 签名；type=1（视频，oid=avid）、type=17（动态，oid=动态数字id）。
    /// 只取第一页（目标的次数 = 请求次数，预算在 collector 侧记账）。
    pub fn replies(
        &mut self,
        oid: &str,
        type_id: i64,
        page_size: i64,
    ) -> Result<Vec<Value>, BilibiliError> {
        if oid.is_empty() || page_size <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/v2/reply",
            &[
                ("type".to_string(), Some(type_id.to_string())),
                ("oid".to_string(), Some(oid.to_string())),
                ("sort".to_string(), Some("2".to_string())),
                ("pn".to_string(), Some("1".to_string())),
                (
                    "ps".to_string(),
                    Some(page_size.clamp(1, REPLIES_PAGE_CAP).to_string()),
                ),
            ],
            false,
            Some("https://www.bilibili.com/"),
        )?;
        Ok(data
            .get("replies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// 直播回放列表（定形：`xlive/web-room/v1/record/getList`，旧 `web-space/space/getLiveRecordList`
    /// 已 404 死亡——S0 晚间探针结论 2026-08-03 复核）。最多翻 MAX_PAGES 页（design 口径 1~2 请求）。
    /// 注意：公开回放浏览 2023 年后全面收紧，多数房间 `count=0` 是**平台现状**，不是参数错误。
    pub fn live_records(
        &mut self,
        room_id: &str,
        page_size: i64,
    ) -> Result<Vec<Value>, BilibiliError> {
        const MAX_PAGES: i64 = 2;
        let page_size = page_size.clamp(1, RECORD_LIST_PAGE_CAP);
        let mut rows: Vec<Value> = Vec::new();
        for page in 1..=MAX_PAGES {
            let data = self.request(
                &self.live_base.clone(),
                "/xlive/web-room/v1/record/getList",
                &[
                    ("room_id".to_string(), Some(room_id.to_string())),
                    ("page".to_string(), Some(page.to_string())),
                    ("page_size".to_string(), Some(page_size.to_string())),
                ],
                false,
                Some(&format!("https://live.bilibili.com/{room_id}")),
            )?;
            let items = data
                .get("list")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let got = items.len();
            rows.extend(items);
            if (got as i64) < page_size {
                break;
            }
        }
        Ok(rows)
    }

    /// 回放弹幕：rid 通道（定形——**不是**稿件 seg.so；seg.so 的 oid 数字会撞旧稿件 cid 产生假阳性）。
    /// 流程：`record/getInfoByLiveRecord?rid` → `data.dm_info.num` 分片数 →
    /// 逐片 `dM/getDMMsgByPlayBackID?rid&index` 收 `data.dm.dm_info[]`（{text, uid, medal...}）。
    /// 每片错误单独记进 elements 的 errors 行？——保持原子：任一片失败整体 Err（上游调用方可按 rid 隔离）。
    pub fn live_record_danmaku(&mut self, rid: &str) -> Result<Vec<Value>, BilibiliError> {
        if rid.is_empty() {
            return Ok(Vec::new());
        }
        let info = self.request(
            &self.live_base.clone(),
            "/xlive/web-room/v1/record/getInfoByLiveRecord",
            &[("rid".to_string(), Some(rid.to_string()))],
            false,
            Some("https://live.bilibili.com/"),
        )?;
        // DANMAKU_SHARD_CAP：无尺循环会放大异常 num 成请求风暴（安全批 R1）。
        let shards = info
            .get("dm_info")
            .and_then(|d| d.get("num"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, DANMAKU_SHARD_CAP);
        let mut messages: Vec<Value> = Vec::new();
        for index in 0..shards {
            let data = self.request(
                &self.live_base.clone(),
                "/xlive/web-room/v1/dM/getDMMsgByPlayBackID",
                &[
                    ("rid".to_string(), Some(rid.to_string())),
                    ("index".to_string(), Some(index.to_string())),
                ],
                false,
                Some("https://live.bilibili.com/"),
            )?;
            let mut chunk = data
                .get("dm")
                .and_then(|d| d.get("dm_info"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // P0-1（迭代细则 v1）：随行打贴本片片号——房间语料 Episode 的幂等身份
            // 是 (rid, shard_index, 行序)，行序靠本循环的按序拼接，片号必须落在行上
            // 才不会跨片漂移。零新增请求：纯内存打标。
            for row in chunk.iter_mut() {
                if let Value::Object(map) = row {
                    map.insert("shard_index".to_string(), Value::from(index));
                }
            }
            messages.extend(chunk);
        }
        Ok(messages)
    }

    /// D1 弹幕网关握手凭据：`getDanmuInfo`（场次窗主源的连接前置）。
    /// 取 `data.host_list[0].{host,port}` 与 `data.token`；任一缺段上抛
    /// `Transport`（细节为固定文案，token/cookie 绝不入错误串——§11 红线）。
    /// 与 Python `bilibili.py` 取 host_list[0] 口径一致。
    pub fn get_danmu_info(&mut self, room_id: &str) -> Result<DanmakuInfo, BilibiliError> {
        const ENDPOINT: &str = "/xlive/web-room/v1/index/getDanmuInfo";
        let data = self.request(
            &self.live_base.clone(),
            ENDPOINT,
            &[("id".to_string(), Some(room_id.to_string()))],
            false,
            Some(&format!("https://live.bilibili.com/{room_id}")),
        )?;
        let first = data
            .get("host_list")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .and_then(Value::as_object);
        let host = first
            .and_then(|row| row.get("host"))
            .and_then(Value::as_str)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| BilibiliError::Transport {
                endpoint: ENDPOINT.to_string(),
                detail: "getDanmuInfo 响应缺 host_list[0].host".to_string(),
            })?
            .to_string();
        let port = first
            .and_then(|row| row.get("port"))
            .and_then(Value::as_i64)
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or(0);
        let token = data
            .get("token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| BilibiliError::Transport {
                endpoint: ENDPOINT.to_string(),
                detail: "getDanmuInfo 响应缺 token".to_string(),
            })?
            .to_string();
        Ok(DanmakuInfo { host, port, token })
    }

    /// D1 场次窗兜底轮询：房间信息接口的 `live_status`
    /// （0 未开播 / 1 直播中 / 2 轮播）。`live_status` 缺席才算错误，
    /// 值 0 是平台的合法「未在播」事实，必须原样返回。
    pub fn get_room_live_status(&mut self, room_id: &str) -> Result<i64, BilibiliError> {
        const ENDPOINT: &str = "/room/v1/Room/get_info";
        let data = self.request(
            &self.live_base.clone(),
            ENDPOINT,
            &[("room_id".to_string(), Some(room_id.to_string()))],
            false,
            Some(&format!("https://live.bilibili.com/{room_id}")),
        )?;
        data.get("live_status")
            .and_then(Value::as_i64)
            .ok_or_else(|| BilibiliError::Transport {
                endpoint: ENDPOINT.to_string(),
                detail: "get_info 响应缺 live_status".to_string(),
            })
    }
}

/// `getDanmuInfo` 返回的弹幕网关凭据（D1：只取 host_list[0] + token）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanmakuInfo {
    /// `host_list[0].host`（原始字符串，未归一）。
    pub host: String,
    /// `host_list[0].port`（取不到按 0 处理，URL 组装回落 wss 标准端）。
    pub port: u16,
    /// 认证 key（op=7 认证包负载的一部分；只进连接，绝不进任何错误串）。
    pub token: String,
}

impl DanmakuInfo {
    /// WS 连接地址：port=443（或取不到）→ `wss://{host}/sub` 标准端点；
    /// 非标准端口（含本地 mock 的随机端口）→ `ws://{host}:{port}/sub`。
    pub fn url(&self) -> String {
        match self.port {
            0 | 443 => format!("wss://{}/sub", self.host),
            port => format!("ws://{}:{}/sub", self.host, port),
        }
    }
}
