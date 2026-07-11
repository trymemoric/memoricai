//! Organization settings repository.

use crate::{db_err, Db};
use memoricai_core::dto::UpdateSettingsRequest;
use memoricai_core::error::Result;
use memoricai_core::model::OrgSettings;
use sqlx::postgres::PgRow;
use sqlx::Row;

fn map_settings(row: &PgRow) -> OrgSettings {
    OrgSettings {
        should_llm_filter: row.get("should_llm_filter"),
        filter_prompt: row.get("filter_prompt"),
        categories: row.get("categories"),
        include_items: row.get("include_items"),
        exclude_items: row.get("exclude_items"),
        chunk_size: row.get("chunk_size"),
    }
}

impl Db {
    pub async fn get_settings(&self, org_id: &str) -> Result<OrgSettings> {
        let row = sqlx::query("SELECT * FROM organization_settings WHERE org_id = $1")
            .bind(org_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_settings).unwrap_or_default())
    }

    pub async fn update_settings(
        &self,
        org_id: &str,
        patch: &UpdateSettingsRequest,
    ) -> Result<OrgSettings> {
        // Read-modify-write to preserve fields not present in the patch.
        let current = self.get_settings(org_id).await?;
        let next = OrgSettings {
            should_llm_filter: patch.should_llm_filter.unwrap_or(current.should_llm_filter),
            filter_prompt: patch.filter_prompt.clone().or(current.filter_prompt),
            categories: patch.categories.clone().or(current.categories),
            include_items: patch.include_items.clone().or(current.include_items),
            exclude_items: patch.exclude_items.clone().or(current.exclude_items),
            chunk_size: patch.chunk_size.unwrap_or(current.chunk_size),
        };
        sqlx::query(
            "INSERT INTO organization_settings
               (org_id, should_llm_filter, filter_prompt, categories, include_items, exclude_items, chunk_size, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7, now())
             ON CONFLICT (org_id) DO UPDATE SET
               should_llm_filter = EXCLUDED.should_llm_filter,
               filter_prompt = EXCLUDED.filter_prompt,
               categories = EXCLUDED.categories,
               include_items = EXCLUDED.include_items,
               exclude_items = EXCLUDED.exclude_items,
               chunk_size = EXCLUDED.chunk_size,
               updated_at = now()",
        )
        .bind(org_id)
        .bind(next.should_llm_filter)
        .bind(&next.filter_prompt)
        .bind(&next.categories)
        .bind(&next.include_items)
        .bind(&next.exclude_items)
        .bind(next.chunk_size)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(next)
    }

    /// Destructive: delete all content for an org (settings reset).
    pub async fn reset_org_data(&self, org_id: &str) -> Result<(u64, u64)> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let m = sqlx::query("DELETE FROM memories WHERE org_id = $1")
            .bind(org_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("DELETE FROM chunks WHERE org_id = $1")
            .bind(org_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        let d = sqlx::query("DELETE FROM documents WHERE org_id = $1")
            .bind(org_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok((d.rows_affected(), m.rows_affected()))
    }
}
