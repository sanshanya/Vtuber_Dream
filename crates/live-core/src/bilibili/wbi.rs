//! WBI 签名纯函数集（Python `bilibili.py` 顶部常量与函数）。
//! 全部；测试 fixture 在 tests-fixtures/m2/parity.json。

/// WBI 混排表（Python MIXIN_KEY_ENC_TAB 原样）。
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

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
pub fn urlencode_pair(key: &str, value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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
}
