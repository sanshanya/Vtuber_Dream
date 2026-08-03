//! B站 HTTP 适配器（移植 Python `bilibili.py`，移植点见 design M2 §bilibili.rs）。
//!
//! 与 Python 的行为差异**仅有明示项**：
//! 1. 不设内建重试：Python 旧实现本无重试，S0 实测 449 请求 0 风控；
//!    延迟节流（request_delay_seconds）+ 预算记账即现有策略，重试策略待真实故障数据再议
//!    （design M2 曾提 reqwest-retry，属预先抽象，记录后搁置——AGENTS.md §4）。
//! 2. 测试接缝：`with_origin` 注入假根地址（生产等价路径用 `new` 直连真地址）；wiremock
//!    负例见同目录测试。
//!
//! 哲学红线：模块只返回**平台原始事实**（serde_json::Value 原样透传），不做任何过滤
//! 之外的语义加工（归一化在 collector.rs，校验在 agent/validators.rs，M3）。

use std::time::{Duration, Instant};

use serde_json::Value;

/// WBI 混排表（Python MIXIN_KEY_ENC_TAB 原样）。
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// Python HIDDEN_CODES：触发隐藏风控的 code 集合（collector 据此记 status="hidden"）。
pub const HIDDEN_CODES: [i64; 4] = [22115, 53013, -403, 10005];

/// Bili 错误分类。`hidden()`：collector 记"隐藏中"而非"失败"。
#[derive(Debug, thiserror::Error)]
pub enum BilibiliError {
    #[error("{endpoint}: http {status}: request failed")]
    Http { endpoint: String, status: u16 },
    #[error("{endpoint}: transport: {detail}")]
    Transport { endpoint: String, detail: String },
    #[error("{endpoint}: non-JSON response")]
    NotJson { endpoint: String },
    #[error("{endpoint}: code={code}: {message}")]
    Api {
        endpoint: String,
        code: i64,
        message: String,
    },
    #[error("{endpoint}: risk verification required")]
    Voucher { endpoint: String },
}

impl BilibiliError {
    pub fn hidden(&self) -> bool {
        matches!(self, Self::Api { code, .. } if HIDDEN_CODES.contains(code))
    }
}

// ---------------------------------------------------------------------------
// 纯函数（直连 tests-fixtures/m2/parity.json，不依赖网络）
// ---------------------------------------------------------------------------

/// Python `_key_from_url`：取 path 的文件名去掉后缀。
pub fn key_from_url(url: &str) -> String {
    let without_query = url.split('?').next().unwrap_or("");
    let file = without_query.rsplit('/').next().unwrap_or("");
    file.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file)
        .to_string()
}

/// Python `_mixin_key`：按混排表取前 32 字符。
pub fn mixin_key(image_key: &str, sub_key: &str) -> String {
    let raw = format!("{image_key}{sub_key}");
    let chars: Vec<char> = raw.chars().collect();
    MIXIN_KEY_ENC_TAB
        .iter()
        .filter_map(|index| chars.get(*index))
        .take(32)
        .collect()
}

/// Python urlencode(quote_via=quote_plus) 对齐：unreserved `A-Za-z0-9_.-~` 原样，
/// 空格→`+`，其余 UTF-8 百分号大写编码。
fn urlencode_pair(key: &str, value: &str) -> String {
    fn encode(text: &str) -> String {
        let mut out = String::new();
        for byte in text.as_bytes() {
            match *byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                    out.push(*byte as char)
                }
                b' ' => out.push('+'),
                _ => {
                    out.push('%');
                    out.push_str(&format!("{byte:02X}"));
                }
            }
        }
        out
    }
    format!("{}={}", encode(key), encode(value))
}

/// Python `sign_wbi`（params 已物化为字符串对；None 剔除由调用方完成）。
/// 过滤规则：值中的 `[''()*` 尽数删除，再 quote_plus 编码；wts=wall 时间戳。
pub fn sign_wbi(
    params: &[(String, Option<String>)],
    mixin: &str,
    timestamp: Option<i64>,
) -> Vec<(String, String)> {
    let wts = timestamp.unwrap_or_else(|| chrono::Utc::now().timestamp());
    let mut signed: Vec<(String, String)> = params
        .iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), sanitize(value))))
        .collect();
    signed.push(("wts".to_string(), wts.to_string()));
    signed.sort_by(|left, right| left.0.cmp(&right.0));
    let query = signed
        .iter()
        .map(|(key, value)| urlencode_pair(key, value))
        .collect::<Vec<_>>()
        .join("&");
    use md5::Digest as _;
    let mut hasher = md5::Md5::new();
    hasher.update(format!("{query}{mixin}").as_bytes());
    let digest = hasher.finalize();
    let mut out = signed;
    out.push(("w_rid".to_string(), format!("{digest:x}")));
    out
}

/// Python `re.sub(r"[!'()*]", "", str(value))`。
fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, '!' | '\'' | '(' | ')' | '*'))
        .collect()
}

// ---------------------------------------------------------------------------
// guard 名单归一化（Python normalize_guard_member）
// ---------------------------------------------------------------------------

fn pick(mapping: &Value, key: &str) -> String {
    mapping
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Python `str(x or "")` 的 or-truthiness：None/0/"" 视为空，其余数字照常转字符串。
fn str_slot(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) if number.as_f64() != Some(0.0) => number.to_string(),
        _ => String::new(),
    }
}

/// Python `str(x or "")`（不 trim——auth_status 级别的原样槽位）。
fn str_or(value: Option<&Value>) -> String {
    str_slot(value)
}

/// Python `bool(x)`：数字/字符串 truthiness（isLogin 可能下发 1）。
fn py_truth(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(t)) => !t.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
        _ => false,
    }
}

/// Python `int(x or 0)`：`int("21")`=21、`int(2.0)`=2、`int(True)`=1、falsy→0。
fn py_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::Bool(true)) => 1,
        Some(Value::String(t)) => t.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// 大航海成员归一化。返回 None = 忽略该条（Python 语义：uid 缺失或为 "0"）。
pub fn normalize_guard_member(item: &Value) -> Option<Vec<(String, Value)>> {
    let uinfo = item.get("uinfo").cloned().unwrap_or_default();
    let base = uinfo.get("base").cloned().unwrap_or(Value::Null);
    let origin = base.get("origin_info").cloned().unwrap_or(Value::Null);
    let medal = uinfo.get("medal").cloned().unwrap_or(Value::Null);
    let guard = uinfo.get("guard").cloned().unwrap_or(Value::Null);

    let uid = str_slot(item.get("uid"));
    let uid = if uid.is_empty() {
        str_slot(uinfo.get("uid"))
    } else {
        uid
    };
    if uid.is_empty() || uid == "0" {
        return None;
    }
    let name = name_chain(item, &base, &origin);
    Some(vec![
        ("uid".to_string(), Value::String(uid)),
        ("name".to_string(), Value::String(name)),
        (
            "face".to_string(),
            Value::String(face_chain(item, &base, &origin)),
        ),
        (
            "guard_level".to_string(),
            first_int_or_none([
                item.get("guard_level"),
                medal.get("guard_level"),
                guard.get("level"),
            ]),
        ),
        (
            "medal_level".to_string(),
            first_int_or_none([
                item.get("medal_level"),
                item.get("level"),
                medal.get("level"),
            ]),
        ),
        (
            "rank".to_string(),
            first_int([item.get("rank"), item.get("user_rank"), None]),
        ),
    ])
}

/// 大航海 rank：`int(rank or user_rank or 0)`——or-链：falsy(0/""/None) 落到下一槽。
fn first_int(candidates: [Option<&Value>; 3]) -> Value {
    for value in candidates {
        match value {
            Some(Value::Null) | None => continue,
            Some(Value::Number(n)) if n.as_f64() == Some(0.0) => continue,
            Some(Value::String(t)) if t.trim().is_empty() => continue,
            Some(Value::Bool(false)) => continue,
            other => return Value::from(py_int(other)),
        }
    }
    Value::from(0)
}

/// 大航海 guard_level/medal_level：Python 是 `is None` 链——只有 None 才落槽；
/// 非 None 值由 `int(x or 0)` 收口（"" → 0，不再尝试下一候选）。
fn first_int_or_none(candidates: [Option<&Value>; 3]) -> Value {
    for value in candidates {
        match value {
            Some(Value::Null) | None => continue,
            other => return Value::from(py_int(other)),
        }
    }
    Value::from(0)
}

fn name_chain(item: &Value, base: &Value, origin: &Value) -> String {
    // Python 的 or 链：username/uname/name/base.name/origin.name（int 槽位不足虑，Python 里仅 str）
    for slot in [
        item.get("username"),
        item.get("uname"),
        item.get("name"),
        base.get("name"),
        origin.get("name"),
    ] {
        let text = str_slot(slot);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

fn face_chain(item: &Value, base: &Value, origin: &Value) -> String {
    for slot in [base.get("face"), origin.get("face"), item.get("face")] {
        let text = str_slot(slot);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Client（reqwest::blocking：与 Python requests 一致的人工节流语义）
// ---------------------------------------------------------------------------

pub struct BilibiliClient {
    http: reqwest::blocking::Client,
    api_base: String,
    live_base: String,
    delay_seconds: f64,
    last_request_started: Option<Instant>,
    request_count: u64,
    nav_cache: Option<Value>,
    mixin_cache: Option<String>,
}

impl BilibiliClient {
    /// 生产入口：直透 bili 官方地址。
    pub fn new(
        cookie: &str,
        delay_seconds: f64,
        timeout_seconds: f64,
    ) -> Result<Self, BilibiliError> {
        Self::with_origin(
            "https://api.bilibili.com",
            "https://api.live.bilibili.com",
            cookie,
            delay_seconds,
            timeout_seconds,
        )
    }

    /// 测试/回放接缝：注入根地址（wiremock 挂载点）。
    pub fn with_origin(
        api_base: &str,
        live_base: &str,
        cookie: &str,
        delay_seconds: f64,
        timeout_seconds: f64,
    ) -> Result<Self, BilibiliError> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in [
            (
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36",
            ),
            ("Origin", "https://www.bilibili.com"),
            ("Accept", "application/json, text/plain, */*"),
            ("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8"),
            ("Cookie", cookie),
        ] {
            headers.insert(
                name.parse::<reqwest::header::HeaderName>()
                    .expect("static header"),
                match value.parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                },
            );
        }
        let http = reqwest::blocking::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs_f64(timeout_seconds.max(1.0)))
            .build()
            .map_err(|err| BilibiliError::Transport {
                endpoint: "<client-build>".to_string(),
                detail: err.to_string(),
            })?;
        Ok(Self {
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
            live_base: live_base.trim_end_matches('/').to_string(),
            delay_seconds: delay_seconds.max(0.0),
            last_request_started: None,
            request_count: 0,
            nav_cache: None,
            mixin_cache: None,
        })
    }

    pub fn request_count(&self) -> u64 {
        self.request_count
    }

    fn throttle(&mut self) {
        if self.delay_seconds > 0.0
            && let Some(last) = self.last_request_started
        {
            let elapsed = last.elapsed().as_secs_f64();
            if elapsed < self.delay_seconds {
                std::thread::sleep(Duration::from_secs_f64(self.delay_seconds - elapsed));
            }
        }
        self.last_request_started = Some(Instant::now());
        self.request_count += 1;
    }

    /// GET + 平台错误分类（412/-352/429 走 Api/Http，v_voucher 单字段 data 走 Voucher）。
    fn request(
        &mut self,
        base: &str,
        path: &str,
        params: &[(String, Option<String>)],
        signed: bool,
        referer: Option<&str>,
    ) -> Result<Value, BilibiliError> {
        let endpoint = path.to_string();
        let query_pairs = if signed {
            sign_wbi(params, &self.mixin_key_sync()?, None)
        } else {
            params
                .iter()
                .filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), value.clone())))
                .collect()
        };
        self.throttle();
        let mut request = self.http.get(format!("{base}{path}"));
        for (key, value) in &query_pairs {
            request = request.query(&[(key.as_str(), value.as_str())]);
        }
        if let Some(referer) = referer {
            request = request.header("Referer", referer.to_string());
        }
        let response = request.send().map_err(|err| BilibiliError::Transport {
            endpoint: endpoint.clone(),
            detail: err.to_string(),
        })?;
        // Python requests.raise_for_status：只有 4xx/5xx 判 HTTP 错误。
        let status = response.status().as_u16();
        if status >= 400 {
            return Err(BilibiliError::Http {
                endpoint: endpoint.clone(),
                status,
            });
        }
        let body: Value = response.json().map_err(|_| BilibiliError::NotJson {
            endpoint: endpoint.clone(),
        })?;
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            // Python `payload.get("message") or payload.get("msg") or "unknown error"`：
            // 空串 message 也回落到 msg（or-truthiness）。
            let pick_message = |key: &str| {
                body.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            let mut message = pick_message("message");
            if message.is_empty() {
                message = pick_message("msg");
            }
            if message.is_empty() {
                message = "unknown error".to_string();
            }
            return Err(BilibiliError::Api {
                endpoint: endpoint.clone(),
                code,
                message,
            });
        }
        // Python：`data = payload.get("data")`——键缺席即 None，由各端点自行归一。
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        // v_voucher：Python 要求真值且单键（null/"" 不构成风控判定）。
        if let Value::Object(map) = &data
            && map.len() == 1
            && matches!(map.get("v_voucher"), Some(v) if py_truth(Some(v)))
        {
            return Err(BilibiliError::Voucher { endpoint });
        }
        Ok(data)
    }

    fn mixin_key_sync(&mut self) -> Result<String, BilibiliError> {
        if let Some(key) = &self.mixin_cache {
            return Ok(key.clone());
        }
        let nav = self.nav()?;
        let image_key = key_from_url(
            nav.get("wbi_img")
                .and_then(|img| img.get("img_url"))
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let sub_key = key_from_url(
            nav.get("wbi_img")
                .and_then(|img| img.get("sub_url"))
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        if image_key.is_empty() || sub_key.is_empty() {
            return Err(BilibiliError::Transport {
                endpoint: "/x/web-interface/nav".to_string(),
                detail: "WBI key is unavailable".to_string(),
            });
        }
        let key = mixin_key(&image_key, &sub_key);
        self.mixin_cache = Some(key.clone());
        Ok(key)
    }

    pub fn nav(&mut self) -> Result<Value, BilibiliError> {
        if let Some(nav) = &self.nav_cache {
            return Ok(nav.clone());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/web-interface/nav",
            &[],
            false,
            None,
        )?;
        self.nav_cache = Some(data.clone());
        Ok(data)
    }

    pub fn auth_status(&mut self) -> Result<Value, BilibiliError> {
        let nav = self.nav()?;
        Ok(serde_json::json!({
            "is_login": py_truth(nav.get("isLogin")),
            "mid": str_or(nav.get("mid")),
            "uname": str_or(nav.get("uname")),
        }))
    }

    /// 大航海名单：top3 + topListNew 分页，去重到 limit（Python 逐行语义，含"页不满即停"）。
    pub fn guard_members(
        &mut self,
        room_id: &str,
        streamer_uid: &str,
        limit: i64,
    ) -> Result<Vec<Value>, BilibiliError> {
        const PAGE_SIZE: i64 = 20;
        let mut members: Vec<Value> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut page = 1;
        while (members.len() as i64) < limit {
            let requested = (limit - members.len() as i64).min(PAGE_SIZE);
            let data = self.request(
                &self.live_base.clone(),
                "/xlive/app-room/v2/guardTab/topListNew",
                &[
                    ("roomid".to_string(), Some(room_id.to_string())),
                    ("ruid".to_string(), Some(streamer_uid.to_string())),
                    ("page".to_string(), Some(page.to_string())),
                    ("page_size".to_string(), Some(requested.to_string())),
                    ("platform".to_string(), Some("web".to_string())),
                    ("typ".to_string(), Some("5".to_string())),
                ],
                false,
                Some(&format!("https://live.bilibili.com/{room_id}")),
            )?;
            let listing = data
                .get("list")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let top3: Vec<Value> = if page == 1 {
                data.get("top3")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            for item in top3.into_iter().chain(listing.iter().cloned()) {
                if let Some(row) = normalize_guard_member(&item) {
                    let uid = row_number_get(&row, "uid").unwrap_or_default();
                    if !uid.is_empty() && seen.insert(uid.clone()) {
                        members.push(Value::Object(row.into_iter().collect()));
                        if (members.len() as i64) >= limit {
                            break;
                        }
                    }
                }
            }
            if (listing.len() as i64) < requested {
                break;
            }
            page += 1;
        }
        members.truncate(limit.max(0) as usize);
        Ok(members)
    }

    pub fn relation_stat(&mut self, uid: &str) -> Result<Value, BilibiliError> {
        self.request(
            &self.api_base.clone(),
            "/x/relation/stat",
            &[("vmid".to_string(), Some(uid.to_string()))],
            false,
            Some(&format!("https://space.bilibili.com/{uid}")),
        )
    }

    pub fn live_room_by_uid(&mut self, uid: &str) -> Result<Value, BilibiliError> {
        self.request(
            &self.live_base.clone(),
            "/room/v1/Room/getRoomInfoOld",
            &[("mid".to_string(), Some(uid.to_string()))],
            false,
            Some(&format!("https://space.bilibili.com/{uid}")),
        )
    }

    pub fn live_room_info(&mut self, room_id: &str) -> Result<Value, BilibiliError> {
        self.request(
            &self.live_base.clone(),
            "/xlive/web-room/v1/index/getInfoByRoom",
            &[("room_id".to_string(), Some(room_id.to_string()))],
            false,
            Some(&format!("https://live.bilibili.com/{room_id}")),
        )
    }

    /// 关注列表：50/页；页不满即停（Python followings 逐行语义）。
    pub fn followings(&mut self, uid: &str, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let mut rows: Vec<Value> = Vec::new();
        let mut page = 1;
        while (rows.len() as i64) < limit {
            let page_size = (limit - rows.len() as i64).min(50);
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
        let data = self.request(
            &self.api_base.clone(),
            "/x/space/wbi/arc/search",
            &[
                ("mid".to_string(), Some(uid.to_string())),
                ("pn".to_string(), Some("1".to_string())),
                ("ps".to_string(), Some(limit.min(30).to_string())),
                ("order".to_string(), Some("pubdate".to_string())),
            ],
            true,
            Some(&format!("https://space.bilibili.com/{uid}/video")),
        )?;
        let list = data
            .get("list")
            .and_then(|list| list.get("vlist"))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(list
            .as_array()
            .map(|rows| rows.iter().take(limit.max(0) as usize).cloned().collect())
            .unwrap_or_default())
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
            let page_size = (limit - rows.len() as i64).min(20);
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
                ("ps".to_string(), Some(limit.min(30).to_string())),
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

    pub fn search_videos(
        &mut self,
        keyword: &str,
        limit: i64,
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
                ("order".to_string(), Some("totalrank".to_string())),
                ("page".to_string(), Some("1".to_string())),
                ("page_size".to_string(), Some(limit.min(20).to_string())),
            ],
            true,
            Some(&format!(
                "https://search.bilibili.com/all?keyword={keyword}"
            )),
        )?;
        Ok(take_items(data.get("result"), limit))
    }

    pub fn hot_searches(&mut self, limit: i64) -> Result<Vec<Value>, BilibiliError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let data = self.request(
            &self.api_base.clone(),
            "/x/web-interface/wbi/search/square",
            &[("limit".to_string(), Some(limit.min(50).to_string()))],
            true,
            Some("https://www.bilibili.com/"),
        )?;
        let trending = data.get("trending").cloned().unwrap_or(Value::Null);
        Ok(take_items(trending.get("list"), limit))
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
                let name = pick(&item, "tag_name").or_fallback(pick(&item, "name"));
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
                ("ps".to_string(), Some(page_size.clamp(1, 20).to_string())),
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
        let page_size = page_size.clamp(1, 20);
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
            for message in chunk {
                if let Value::Object(mut row) = message {
                    row.insert("shard_index".to_string(), Value::from(index));
                    messages.push(Value::Object(row));
                }
            }
        }
        Ok(messages)
    }
}

trait OrFallback {
    fn or_fallback(self, fallback: String) -> String;
}
impl OrFallback for String {
    fn or_fallback(self, fallback: String) -> String {
        if self.is_empty() { fallback } else { self }
    }
}

/// 列表族端点统一收编：Python `[item for item in … or [] if isinstance(item, dict)][:limit]`
/// ——先过滤非 dict 条目（垃圾条目既不入列、也不参与"页不满"判定）。
fn take_items(value: Option<&Value>, limit: i64) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(Value::is_object)
        .take(limit.max(0) as usize)
        .collect()
}

fn row_number_get(row: &[(String, Value)], key: &str) -> Option<String> {
    row.iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_parity_key_and_defaults() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests-fixtures/m2/parity.json");
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(root).unwrap()).unwrap();
        let keys = &fixture["mixin_key"];
        assert_eq!(
            key_from_url(keys["image_url"].as_str().unwrap()),
            keys["image_key"]
        );
        assert_eq!(
            key_from_url(keys["sub_url"].as_str().unwrap()),
            keys["sub_key"]
        );
        assert_eq!(
            mixin_key(
                keys["image_key"].as_str().unwrap(),
                keys["sub_key"].as_str().unwrap()
            ),
            keys["expected"].as_str().unwrap(),
            "Python mixin_key 字节对账",
        );
        for case in fixture["sign_wbi"].as_array().unwrap() {
            let params: Vec<(String, Option<String>)> = case["params"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        match value {
                            Value::Null => None,
                            other => Some(match other {
                                Value::String(text) => text.clone(),
                                Value::Number(n) => n.to_string(),
                                other => other.to_string(),
                            }),
                        },
                    )
                })
                .collect();
            let signed = sign_wbi(
                &params,
                keys["expected"].as_str().unwrap().to_string().as_str(),
                case["timestamp"].as_i64(),
            );
            let expected = case["expected"].as_object().unwrap();
            let mut map = signed
                .iter()
                .cloned()
                .collect::<std::collections::BTreeMap<_, _>>();
            map.remove("w_rid");
            for (key, value) in expected {
                if key != "w_rid" {
                    assert_eq!(
                        map.remove(key).unwrap(),
                        value.as_str().unwrap(),
                        "sign_wbi 字段 {key}（params {:?}）",
                        case["params"],
                    );
                }
            }
            assert!(map.is_empty(), "sign_wbi 多签了字段 {map:?}");
            assert_eq!(
                signed.iter().find(|(k, _)| k == "w_rid").unwrap().1,
                expected["w_rid"].as_str().unwrap(),
                "Python MD5 指纹（params {:?}）",
                case["params"],
            );
        }
    }

    #[test]
    fn normalize_guard_member_parity_fixture() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests-fixtures/m2/parity.json");
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(root).unwrap()).unwrap();
        for case in fixture["guard_member"].as_array().unwrap() {
            let expected = &case["expected"];
            let actual = normalize_guard_member(&case["input"])
                .map(|row| row.into_iter().collect::<serde_json::Map<_, _>>())
                .map(Value::Object)
                .unwrap_or(Value::Null);
            if expected.is_null() {
                assert!(actual.is_null(), "ignore-case 应丢: {:?}", case["input"]);
            } else {
                assert_eq!(&actual, expected, "guard 归一化: {:?}", case["input"]);
            }
        }
    }

    #[test]
    fn hidden_codes_match_python() {
        for code in HIDDEN_CODES {
            assert!(
                BilibiliError::Api {
                    endpoint: String::new(),
                    code,
                    message: String::new()
                }
                .hidden()
            );
        }
        assert!(
            !BilibiliError::Api {
                endpoint: String::new(),
                code: -352,
                message: String::new()
            }
            .hidden()
        );
    }
}
