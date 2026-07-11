//! Profile buckets, bucket assignment on memories, and profile summaries.

use crate::{db_err, map_memory, Db};
use memoricai_core::error::Result;
use memoricai_core::model::{Memory, ProfileBucket};
use sqlx::Row;

impl Db {
    pub async fn create_bucket(
        &self,
        org_id: &str,
        container_tag: Option<&str>,
        key: &str,
        description: &str,
    ) -> Result<ProfileBucket> {
        sqlx::query(
            "INSERT INTO profile_buckets (id, org_id, container_tag, key, description)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (org_id, coalesce(container_tag, ''), key)
             DO UPDATE SET description = EXCLUDED.description",
        )
        .bind(memoricai_core::ids::token(21))
        .bind(org_id)
        .bind(container_tag)
        .bind(key)
        .bind(description)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(ProfileBucket {
            key: key.to_string(),
            description: description.to_string(),
        })
    }

    /// Union of org-level and (optionally) space-level buckets, plus the built-in `preferences`.
    pub async fn list_buckets(
        &self,
        org_id: &str,
        container_tag: Option<&str>,
    ) -> Result<Vec<ProfileBucket>> {
        let rows = sqlx::query(
            "SELECT DISTINCT key, description FROM profile_buckets
             WHERE org_id = $1 AND (container_tag IS NULL OR container_tag = $2)
             ORDER BY key",
        )
        .bind(org_id)
        .bind(container_tag)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let mut buckets: Vec<ProfileBucket> = rows
            .iter()
            .map(|r| ProfileBucket {
                key: r.get("key"),
                description: r.get("description"),
            })
            .collect();
        if !buckets.iter().any(|b| b.key == "preferences") {
            buckets.insert(
                0,
                ProfileBucket {
                    key: "preferences".into(),
                    description: "User preferences and settings.".into(),
                },
            );
        }
        Ok(buckets)
    }

    pub async fn set_memory_bucket(&self, memory_id: &str, bucket_key: &str) -> Result<()> {
        sqlx::query("UPDATE memories SET bucket_key = $2 WHERE id = $1")
            .bind(memory_id)
            .bind(bucket_key)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    pub async fn memories_in_bucket(
        &self,
        org_id: &str,
        container_tag: &str,
        bucket_key: &str,
        limit: i64,
    ) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT * FROM memories WHERE org_id = $1 AND space_container_tag = $2
             AND bucket_key = $3 AND is_latest AND NOT is_forgotten
             ORDER BY created_at DESC LIMIT $4",
        )
        .bind(org_id)
        .bind(container_tag)
        .bind(bucket_key)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_memory).collect())
    }

    pub async fn upsert_profile_summary(
        &self,
        org_id: &str,
        container_tag: &str,
        bucket_key: Option<&str>,
        summary: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO profile_summaries (id, org_id, container_tag, bucket_key, summary, updated_at)
             VALUES ($1,$2,$3,$4,$5, now())
             ON CONFLICT (org_id, container_tag, coalesce(bucket_key, ''))
             DO UPDATE SET summary = EXCLUDED.summary, updated_at = now()",
        )
        .bind(memoricai_core::ids::token(21))
        .bind(org_id)
        .bind(container_tag)
        .bind(bucket_key)
        .bind(summary)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// (bucket_key, summary) pairs; bucket_key is None for the general summary.
    pub async fn get_profile_summaries(
        &self,
        org_id: &str,
        container_tag: &str,
    ) -> Result<Vec<(Option<String>, String)>> {
        let rows = sqlx::query(
            "SELECT bucket_key, summary FROM profile_summaries
             WHERE org_id = $1 AND container_tag = $2",
        )
        .bind(org_id)
        .bind(container_tag)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<Option<String>, _>("bucket_key"), r.get("summary")))
            .collect())
    }

    /// Memories eligible for `[Summary]` aggregation (older than `older_than_days`).
    pub async fn aggregatable_memories(
        &self,
        org_id: &str,
        container_tag: &str,
        older_than_days: i64,
        limit: i64,
    ) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT * FROM memories WHERE org_id = $1 AND space_container_tag = $2
             AND is_latest AND NOT is_forgotten AND NOT is_static
             AND created_at < now() - ($3 || ' days')::interval
             ORDER BY created_at ASC LIMIT $4",
        )
        .bind(org_id)
        .bind(container_tag)
        .bind(older_than_days.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_memory).collect())
    }
}
