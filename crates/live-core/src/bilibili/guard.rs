//! guard（大航海名单）归一化。
//! `normalize_guard_member` 经 [name/face/int] 链从 deep nested item 提取，fixture 在 parity.json。

use serde_json::Value;

use super::{py_int, str_slot};

// ---------------------------------------------------------------------------
// guard 名单归一化（Python normalize_guard_member）
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
