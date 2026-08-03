/// `BilibiliClient` 的端点续接实现（bilibili/mod.rs 行数上限拆分）。
/// 全部方法依赖 mod.rs 的 request()/throttle()/nav()/mixin_key_sync() 等共享面。
use serde_json::Value;

use super::{
    BANGUMI_PAGE_CAP, BilibiliClient, BilibiliError, FAVORITE_ITEMS_PAGE_CAP, FOLLOWINGS_PAGE_CAP,
    HOT_SEARCHES_LIMIT_CAP, RECORD_LIST_PAGE_CAP, REPLIES_PAGE_CAP, SEARCH_VIDEOS_PAGE_SIZE,
    VIDEOS_PAGE_CAP, pick, py_int, py_truth, take_items,
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
        let shards = info
            .get("dm_info")
            .and_then(|d| d.get("num"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let mut messages: Vec<Value> = Vec::new();
        for index in 0..shards.max(0) {
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
            let chunk = data
                .get("dm")
                .and_then(|d| d.get("dm_info"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            messages.extend(chunk);
        }
        Ok(messages)
    }
}
