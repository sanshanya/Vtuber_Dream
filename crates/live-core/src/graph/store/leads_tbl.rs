//! G2（design §9.2 行 254）：discovery_leads 表的受控写入/读出。
//!
//! 账面语义与 M4.x JSONL 账本逐字同源：
//! - `insert_lead_rows(strict=false)` 是幂等闸门（INSERT OR IGNORE，同 dedupe_key
//!   任意状态跳行）；`strict=true` 是唯一键本体验证通道（违例即以 Err 面世）；
//! - 行序 = rowid（= 写账序，JSONL 文件序的同构物：summary 的 latest_consumed
//!   倒取依赖它）；
//! - `update_lead_row` 是状态机/消费留痕的唯一写回口（全字段覆盖写，键不变）。

use rusqlite::{OptionalExtension, params};

use crate::leads::{LedgerRow, status_from_name, status_name};

use super::{Result, Store, repo_err};

/// 轮2-R1-B2：evidence_ids 序列化口径公共件（insert/update 双写点同修一份）。
fn evidence_ids_json(row: &LedgerRow) -> Result<String> {
    serde_json::to_string(&row.evidence_ids)
        .map_err(|err| super::StoreError::Repo(format!("evidence_ids 不可序列化：{err}")))
}

impl Store {
    /// 入账。返回实际入库行数（OR IGNORE 臂下同键行不计）。
    pub fn insert_lead_rows(&self, rows: &[&LedgerRow], strict: bool) -> Result<usize> {
        let verb = if strict { "INSERT" } else { "INSERT OR IGNORE" };
        let sql = format!(
            "{verb} INTO discovery_leads(\
               dedupe_key,lead_type,locator,motivation,expected_signal,priority,\
               evidence_ids_json,viewer_id,first_seen_run_id,created_at,status,\
               yield_count,resolution_note) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)"
        );
        let mut inserted = 0_usize;
        for row in rows {
            let evidence_ids = evidence_ids_json(row)?;
            inserted += self.conn.execute(
                &sql,
                params![
                    row.dedupe_key,
                    row.lead_type,
                    row.locator,
                    row.motivation,
                    row.expected_signal,
                    row.priority,
                    evidence_ids,
                    row.viewer_id,
                    row.first_seen_run_id,
                    row.created_at,
                    status_name(row.status),
                    row.yield_count,
                    row.resolution_note,
                ],
            )?;
        }
        Ok(inserted)
    }

    fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerRow> {
        let evidence_ids_json: String = row.get(6)?;
        let status_text: String = row.get(10)?;
        let evidence_ids = serde_json::from_str(&evidence_ids_json).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(err))
        })?;
        let status = status_from_name(&status_text).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                format!("未知线索状态 {status_text}").into(),
            )
        })?;
        Ok(LedgerRow {
            dedupe_key: row.get(0)?,
            lead_type: row.get(1)?,
            locator: row.get(2)?,
            motivation: row.get(3)?,
            expected_signal: row.get(4)?,
            priority: row.get(5)?,
            evidence_ids,
            viewer_id: row.get(7)?,
            first_seen_run_id: row.get(8)?,
            created_at: row.get(9)?,
            status,
            yield_count: row.get(11)?,
            resolution_note: row.get(12)?,
        })
    }

    const LEAD_SELECT: &'static str = "SELECT \
         dedupe_key,lead_type,locator,motivation,expected_signal,priority,\
         evidence_ids_json,viewer_id,first_seen_run_id,created_at,status,\
         yield_count,resolution_note FROM discovery_leads";

    /// 全账（写账序）。读面唯一入口——JSONL 读面的同构物。
    pub fn lead_rows(&self) -> Result<Vec<LedgerRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{} ORDER BY rowid", Self::LEAD_SELECT))?;
        let rows = stmt
            .query_map([], Self::row_from_sql)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn lead_row(&self, dedupe_key: &str) -> Result<Option<LedgerRow>> {
        self.conn
            .query_row(
                &format!("{} WHERE dedupe_key=?", Self::LEAD_SELECT),
                params![dedupe_key],
                Self::row_from_sql,
            )
            .optional()
            .map_err(Into::into)
    }

    /// 状态机/消费留痕写回。dedupe_key 身份不变，其余全字段覆盖写。
    pub fn update_lead_row(&self, row: &LedgerRow) -> Result<()> {
        let evidence_ids = evidence_ids_json(row)?;
        let changed = self.conn.execute(
            "UPDATE discovery_leads SET lead_type=?,locator=?,motivation=?,expected_signal=?,\
             priority=?,evidence_ids_json=?,viewer_id=?,first_seen_run_id=?,created_at=?,\
             status=?,yield_count=?,resolution_note=? WHERE dedupe_key=?",
            params![
                row.lead_type,
                row.locator,
                row.motivation,
                row.expected_signal,
                row.priority,
                evidence_ids,
                row.viewer_id,
                row.first_seen_run_id,
                row.created_at,
                status_name(row.status),
                row.yield_count,
                row.resolution_note,
                row.dedupe_key,
            ],
        )?;
        if changed == 0 {
            return repo_err(format!("lead {} 不存在，写回拒绝", row.dedupe_key));
        }
        Ok(())
    }
}
