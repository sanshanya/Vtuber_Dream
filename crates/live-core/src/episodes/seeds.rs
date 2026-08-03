//! 确定式 mention seeds（书名号/话题号等显式表面候选）+ 字符 span 校验。
//! 语义裁决仍在 Agent；这里只提供可回放的最小事实候选。

use super::*;

// ---------------------------------------------------------------------------
// 确定式 mention seeds 与 span 校验
// ---------------------------------------------------------------------------

/// 字节偏移 → 字符偏移（regex 返回字节，Python 语义是字符）。
fn byte_to_char_offset(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset].chars().count()
}

/// `deterministic_mention_seeds`：只提取显式表面候选；语义裁决仍在 Agent。
pub fn deterministic_mention_seeds(episodes: &[Episode]) -> Vec<Value> {
    let mut seeds: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, usize, usize)> =
        std::collections::HashSet::new();

    let mut add = |episode: &Episode,
                   field: &EpisodeField,
                   start: usize,
                   end: usize,
                   text: &str,
                   kind: &str,
                   origin: &str| {
        let key = (episode.episode_id.clone(), field.path.clone(), start, end);
        if seen.contains(&key) || text.trim().is_empty() {
            return;
        }
        seen.insert(key.clone());
        let seed_id = format!(
            "seed:{}",
            hash_parts(
                &[
                    episode.episode_id.clone(),
                    field.path.clone(),
                    py_str_int(start as i64),
                    py_str_int(end as i64),
                ],
                20,
            )
        );
        let mut seed = Map::new();
        seed.insert("seed_id".to_string(), Value::String(seed_id));
        seed.insert(
            "episode_id".to_string(),
            Value::String(episode.episode_id.clone()),
        );
        seed.insert("field_path".to_string(), Value::String(field.path.clone()));
        seed.insert("text".to_string(), Value::String(text.to_string()));
        seed.insert("start".to_string(), Value::from(start as i64));
        seed.insert("end".to_string(), Value::from(end as i64));
        seed.insert("surface_kind".to_string(), Value::String(kind.to_string()));
        seed.insert("origin".to_string(), Value::String(origin.to_string()));
        seeds.push(Value::Object(seed));
    };

    for episode in episodes {
        for field in &episode.fields {
            if field.kind.starts_with("platform_") {
                add(
                    episode,
                    field,
                    0,
                    char_len(&field.text),
                    &field.text.clone(),
                    &field.kind,
                    "platform",
                );
            }
            if field.kind != "text" {
                continue;
            }
            for pattern in QUOTED_PATTERNS.iter() {
                for capture in pattern.captures_iter(&field.text) {
                    if let Some(spot) = capture.get(1) {
                        add(
                            episode,
                            field,
                            byte_to_char_offset(&field.text, spot.start()),
                            byte_to_char_offset(&field.text, spot.end()),
                            spot.as_str(),
                            "quoted_expression",
                            "explicit",
                        );
                    }
                }
            }
            // 平台 tag 在自由文本中的重现定位（展示高亮用）。
            if let Value::Array(tags) = get_value(&episode.platform_facts, "tags") {
                for tag_value in tags {
                    let tag = py_str(tag_value);
                    if tag.is_empty() {
                        continue;
                    }
                    let mut search_from = 0usize; // 字符偏移
                    while let Some(relative) =
                        char_slice(&field.text, search_from, char_len(&field.text)).find(&tag)
                    {
                        // char_slice 复制子串，find 返回子串内字节偏移 → 转字符
                        let index = search_from
                            + byte_to_char_offset(
                                &char_slice(&field.text, search_from, char_len(&field.text)),
                                relative,
                            );
                        add(
                            episode,
                            field,
                            index,
                            index + char_len(&tag),
                            &tag,
                            "platform_tag_in_text",
                            "platform",
                        );
                        search_from = index + char_len(&tag);
                    }
                }
            }
        }
    }
    seeds
}

/// `validate_span`：失败时返回与 Python 相同的错误文案。
pub fn validate_span(
    episode: &Episode,
    field_path: &str,
    text: &str,
    start: i64,
    end: i64,
) -> Option<String> {
    let source = match episode.field_text(field_path) {
        Some(source) => source,
        None => {
            return Some(format!(
                "episode {} has no field {}",
                episode.episode_id, field_path
            ));
        }
    };
    let total = char_len(source) as i64;
    if start < 0 || end > total || end <= start {
        return Some(format!(
            "invalid offsets for {}:{}",
            episode.episode_id, field_path
        ));
    }
    let actual = char_slice(source, start as usize, end as usize);
    if actual != text {
        return Some(format!(
            "span mismatch for {}:{}; expected exact substring {:?}, got {:?}",
            episode.episode_id, field_path, actual, text
        ));
    }
    None
}
