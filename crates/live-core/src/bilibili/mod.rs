//! B站 HTTP 适配器（移植 Python `bilibili.py`，移植点见 design M2 §bilibili.rs）。
//!
//! 与 Python 的行为差异**仅有明示项**：
//! 1. 不设内建重试：Python 旧实现本无重试，S0 实测 449 请求 0 风控；
//!    延迟节流（request_delay_seconds）+ 预算记账即现有策略，重试策略待真实故障数据再议
//!    （design M2 曾提 reqwest-retry，属预先抽象，记录后搁置——AGENTS.md 代码简化规则）。
//! 2. 测试接缝：`with_origin` 注入假根地址（生产等价路径用 `new` 直连真地址）；wiremock
//!    负例见同目录测试。
//!
//! 哲学红线：模块只返回**平台原始事实**（serde_json::Value 原样透传），不做任何过滤
//! 之外的语义加工（归一化在 collector/mod.rs，校验在 agent/validators.rs，M3）。

use std::time::{Duration, Instant};

use serde_json::Value;

pub(crate) mod endpoints;
pub(crate) mod guard;
pub(crate) mod wbi;

pub(crate) use wbi::{key_from_url, mixin_key, sign_wbi};

/// 分页硬尺子（design 文档"任何阈值必须有命名"；clamp 意图写进各自的调用处注释）。
pub const FOLLOWINGS_PAGE_CAP: i64 = 50;
pub const FAVORITE_ITEMS_PAGE_CAP: i64 = 20;
pub const VIDEOS_PAGE_CAP: i64 = 30;
pub const BANGUMI_PAGE_CAP: i64 = 30;
pub const HOT_SEARCHES_LIMIT_CAP: i64 = 50;
pub const REPLIES_PAGE_CAP: i64 = 20;
pub const RECORD_LIST_PAGE_CAP: i64 = 20;
/// 回放弹幕分片数硬尺：响应 dm_info.num 直接驱动逐片请求循环；
/// 正常一场回放分片数十级，200 是防异常响应请求放大的防御上界（Rust 自定，Python 无对应端点）。
pub const DANMAKU_SHARD_CAP: i64 = 200;
/// 搜索接口单页条数（Python `min(limit, 20)`，search/type 单页上限）。
pub const SEARCH_VIDEOS_PAGE_SIZE: i64 = 20;

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
// Python 标量语义 helpers（pick/str_slot/str_or/py_truth/py_int）
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
            let page_before = members.len();
            for item in top3.into_iter().chain(listing.iter().cloned()) {
                if let Some(row) = guard::normalize_guard_member(&item) {
                    let uid = row_number_get(&row, "uid").unwrap_or_default();
                    if !uid.is_empty() && seen.insert(uid.clone()) {
                        members.push(Value::Object(row.into_iter().collect()));
                        if (members.len() as i64) >= limit {
                            break;
                        }
                    }
                }
            }
            // 轮2-R1-A②：收杆判据必须含「本轮零新增」——normalize 落 none（uid 脏/0）
            // 或全撞 seen 时 members 不长，listing 满页也照滚；修前据此判定 page 无界
            // 自增对服务器灌包（钉：guard_members_full_pages_of_junk…恰好 1 请求）。
            // 语义边界：满页但有新增 → 续页有正当预期；满页零新增 → 后续页只会更重走
            // 同一片脏区（top3 只在 page=1，seen 只增不减）。
            if members.len() == page_before || (listing.len() as i64) < requested {
                break;
            }
            page += 1;
        }
        members.truncate(limit.max(0) as usize);
        Ok(members)
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
