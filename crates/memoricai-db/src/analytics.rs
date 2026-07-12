//! Analytics aggregation queries over `api_requests`.

use crate::{count_and_rows, db_err, Db};
use memoricai_core::error::Result;
use sqlx::Row;

pub struct UsageRow {
    pub kind: String,
    pub count: i64,
    pub avg_duration: f64,
}

pub struct LogRow {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub kind: String,
    pub status_code: Option<i32>,
    pub duration: Option<i64>,
}

impl Db {
    /// Request counts + avg duration grouped by type within the last `days`.
    pub async fn usage_by_type(&self, org_id: &str, days: i64) -> Result<Vec<UsageRow>> {
        let rows = sqlx::query(
            "SELECT type, count(*) AS c, coalesce(avg(duration),0)::float8 AS avg_d
             FROM api_requests
             WHERE org_id = $1 AND created_at > now() - ($2 || ' days')::interval
             GROUP BY type ORDER BY c DESC",
        )
        .bind(org_id)
        .bind(days.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|r| UsageRow {
                kind: r.get("type"),
                count: r.get("c"),
                avg_duration: r.get::<f64, _>("avg_d"),
            })
            .collect())
    }

    pub async fn total_memories(&self, org_id: &str) -> Result<i64> {
        let c: i64 = sqlx::query(
            "SELECT count(*) AS c FROM memories WHERE org_id = $1 AND is_latest AND NOT is_forgotten",
        )
        .bind(org_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?
        .get("c");
        Ok(c)
    }

    /// (total_requests, total_errors, [(status_code, count)]) within `days`.
    pub async fn error_stats(
        &self,
        org_id: &str,
        days: i64,
    ) -> Result<(i64, i64, Vec<(i32, i64)>)> {
        let total_q = sqlx::query(
            "SELECT count(*) AS c FROM api_requests
             WHERE org_id = $1 AND created_at > now() - ($2 || ' days')::interval",
        )
        .bind(org_id)
        .bind(days.to_string());

        let rows_q = sqlx::query(
            "SELECT status_code, count(*) AS c FROM api_requests
             WHERE org_id = $1 AND status_code >= 400
               AND created_at > now() - ($2 || ' days')::interval
             GROUP BY status_code ORDER BY c DESC",
        )
        .bind(org_id)
        .bind(days.to_string());

        let (total, rows) = count_and_rows(&self.pool, total_q, rows_q).await?;
        let by_status: Vec<(i32, i64)> = rows
            .iter()
            .map(|r| {
                (
                    r.get::<Option<i32>, _>("status_code").unwrap_or(0),
                    r.get::<i64, _>("c"),
                )
            })
            .collect();
        let total_errors: i64 = by_status.iter().map(|(_, c)| c).sum();
        Ok((total, total_errors, by_status))
    }

    pub async fn request_logs(
        &self,
        org_id: &str,
        page: u32,
        limit: u32,
    ) -> Result<(Vec<LogRow>, i64)> {
        let offset = (page.saturating_sub(1) as i64) * limit as i64;
        let total_q =
            sqlx::query("SELECT count(*) AS c FROM api_requests WHERE org_id = $1").bind(org_id);
        let rows_q = sqlx::query(
            "SELECT id, created_at, type, status_code, duration FROM api_requests
             WHERE org_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(org_id)
        .bind(limit as i64)
        .bind(offset);
        let (total, rows) = count_and_rows(&self.pool, total_q, rows_q).await?;
        let logs = rows
            .iter()
            .map(|r| LogRow {
                id: r.get("id"),
                created_at: r.get("created_at"),
                kind: r.get("type"),
                status_code: r.get("status_code"),
                duration: r.get("duration"),
            })
            .collect();
        Ok((logs, total))
    }
}
