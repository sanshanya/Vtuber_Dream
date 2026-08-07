//! 事实层写入：不可变 Episode + Mention（含 REFERS_TO 区间闭合）。

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};

use crate::episodes::{Episode, EpisodeField, hash_parts, json_canon};

use super::{Result, Store, repo_err};

impl Store {
    // --------------------------------------------------------------- episode

    pub fn upsert_episode(&self, episode: &Episode) -> Result<()> {
        self.upsert_episode_inner(episode, None)
    }

    /// G2（design §8.4/§9.2）：线索出产的 Episode 带 lead_id（→discovery_leads
    /// 外键，溯源链「线索 → episode」）。非线索出产照旧 None——episodes.lead_id
    /// 列 NULL。幂等复检臂（同 id 同 hash 只刷 last_seen）绝不触碰既有挂链。
    pub fn upsert_episode_with_lead(&self, episode: &Episode, lead_id: Option<&str>) -> Result<()> {
        self.upsert_episode_inner(episode, lead_id)
    }

    fn upsert_episode_inner(&self, episode: &Episode, lead_id: Option<&str>) -> Result<()> {
        let now = self.now();
        let fields_value = Value::Array(episode.fields.iter().map(EpisodeField::to_json).collect());
        let content_hash = hash_parts(
            &[json_canon(&serde_json::json!({
                "viewer_id": episode.viewer_id,
                "source": episode.source,
                "event_type": episode.event_type,
                "published_at": episode.published_at,
                "title": episode.title,
                "url": episode.url,
                "bvid": episode.bvid,
                "fields": fields_value,
                "platform_facts": episode.platform_facts,
            }))],
            40,
        );
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT content_hash FROM episodes WHERE episode_id=?",
                params![episode.episode_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(old_hash) = existing {
            if old_hash != content_hash {
                return repo_err(format!(
                    "immutable Episode conflict: {}",
                    episode.episode_id
                ));
            }
            self.conn.execute(
                "UPDATE episodes SET last_seen_at=? WHERE episode_id=?",
                params![now, episode.episode_id],
            )?;
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO episodes(\
               episode_id,viewer_id,source,event_type,observed_at,published_at,title,url,bvid,\
               fields_json,platform_facts_json,content_hash,first_seen_at,last_seen_at,lead_id) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                episode.episode_id,
                episode.viewer_id,
                episode.source,
                episode.event_type,
                episode.observed_at,
                episode.published_at,
                episode.title,
                episode.url,
                episode.bvid,
                json_canon(&fields_value),
                json_canon(&episode.platform_facts),
                content_hash,
                now,
                now,
                lead_id
            ],
        )?;
        Ok(())
    }

    /// D1-2B 身份面收拢（同窗续接幂等复检臂）：WS 行 identity = stable 前缀
    /// `episode:{viewer}:{stable}`，**不含** content_version 尾段。理由：WS 场次事实
    /// （session rid / 窗口起点）随附着时刻漂移，背靠背两次 collect（或同窗断线重发）
    /// 采到同一条平台行（同 ts/uid/文本）会得到不同 content_version → full-id 撞库
    /// 完全失效、同一事实重复入账并虚增复盘四个数。归位规则与 upsert 复检臂同族：
    /// 同身份已存在 → 只刷 last_seen_at（行事实保首次所见，追加不覆盖）。
    ///
    /// immutable 守卫不降格：命中时比对 fields 指纹——同身份不同正文照旧报
    /// immutable Episode conflict（绝不静默覆盖事实）。
    ///
    /// 只为 WS 线调用：replay/comment 语料的 stable 嵌平台 id、version 天然稳定，
    /// 其「同 stable 不同内容」必须继续走 full-id 冲突报错面（immutable 守卫）。
    pub fn touch_episode_by_identity(
        &self,
        identity_prefix: &str,
        fields_json_canon: &str,
    ) -> Result<bool> {
        let now = self.now();
        // ASCII 后继区间：':' (0x3A) < ';' (0x3B)——[lo, hi) 恰为「以此串打头」的前缀集。
        let lo = format!("{identity_prefix}:");
        let hi = format!("{identity_prefix};");
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT fields_json FROM episodes WHERE episode_id >= ? AND episode_id < ? \
                 ORDER BY episode_id LIMIT 1",
                params![lo, hi],
                |row| row.get(0),
            )
            .optional()?;
        let Some(old_fields) = existing else {
            return Ok(false);
        };
        if old_fields != fields_json_canon {
            return repo_err(format!(
                "immutable Episode conflict: {identity_prefix}:*（同身份不同正文）"
            ));
        }
        self.conn.execute(
            "UPDATE episodes SET last_seen_at=? WHERE episode_id >= ? AND episode_id < ?",
            params![now, lo, hi],
        )?;
        Ok(true)
    }

    // -------------------------------------------------------------- mentions

    pub fn upsert_mention(
        &self,
        mention: &crate::models::MentionSpan,
        viewer_id: &str,
        run_id: &str,
        resolved_entity_id: Option<&str>,
        decision: &str,
    ) -> Result<String> {
        let mention_id = mention_id_of(viewer_id, mention);
        let now = self.now();
        self.conn.execute(
            "INSERT INTO mentions(\
               mention_id,episode_id,viewer_id,field_path,text,start_offset,end_offset,mention_type,\
               origin,proposed_entity_name,proposed_entity_type,confidence,run_id,created_at) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) \
             ON CONFLICT(mention_id) DO UPDATE SET confidence=excluded.confidence,run_id=excluded.run_id",
            params![
                mention_id,
                mention.episode_id,
                viewer_id,
                mention.field_path,
                mention.text,
                mention.start,
                mention.end,
                mention.mention_type,
                if mention.origin.is_empty() {
                    "explicit"
                } else {
                    mention.origin.as_str()
                },
                mention.proposed_entity_name,
                mention.proposed_entity_type,
                mention.confidence,
                run_id,
                now
            ],
        )?;
        let mut node_props = Map::new();
        node_props.insert(
            "episode_id".to_string(),
            Value::String(mention.episode_id.clone()),
        );
        node_props.insert(
            "field_path".to_string(),
            Value::String(mention.field_path.clone()),
        );
        node_props.insert("start".to_string(), Value::from(mention.start));
        node_props.insert("end".to_string(), Value::from(mention.end));
        node_props.insert(
            "origin".to_string(),
            if mention.origin.is_empty() {
                Value::Null
            } else {
                Value::String(mention.origin.clone())
            },
        );
        self.upsert_node(
            &mention_id,
            "Mention",
            &mention.text,
            &Value::Object(node_props),
            "grounded_ai",
            None,
        )?;
        self.upsert_edge(
            &mention.episode_id,
            "CONTAINS_MENTION",
            &mention_id,
            &serde_json::json!({}),
            "grounded_ai",
            Some(mention.confidence),
            std::slice::from_ref(&mention_id),
            run_id,
            None,
        )?;
        if let Some(resolved) = resolved_entity_id {
            self.conn.execute(
                "UPDATE edges SET valid_to=?,last_seen_at=? \
                 WHERE source_id=? AND predicate='REFERS_TO' AND source_kind='grounded_ai' \
                 AND target_id<>? AND valid_to IS NULL",
                params![now, now, mention_id, resolved],
            )?;
            self.upsert_edge(
                &mention_id,
                "REFERS_TO",
                resolved,
                &serde_json::json!({"decision": decision}),
                "grounded_ai",
                Some(mention.confidence),
                std::slice::from_ref(&mention_id),
                run_id,
                None,
            )?;
        } else {
            self.conn.execute(
                "UPDATE edges SET valid_to=?,last_seen_at=? \
                 WHERE source_id=? AND predicate='REFERS_TO' AND source_kind='grounded_ai' \
                 AND valid_to IS NULL",
                params![now, now, mention_id],
            )?;
        }
        Ok(mention_id)
    }
}

/// `mention:{viewer}:{hash24(episode_id, field_path, start, end, text)}`
/// （start/end 走 Python int-or-""：0 → ""）。
pub fn mention_id_of(viewer_id: &str, mention: &crate::models::MentionSpan) -> String {
    format!(
        "mention:{}:{}",
        viewer_id,
        hash_parts(
            &[
                mention.episode_id.clone(),
                mention.field_path.clone(),
                crate::episodes::py_str_int(mention.start),
                crate::episodes::py_str_int(mention.end),
                mention.text.clone(),
            ],
            24,
        )
    )
}
