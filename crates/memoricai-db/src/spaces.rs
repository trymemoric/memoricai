//! Space / project (container-tag) repository.

use crate::{db_err, Db};
use memoricai_core::enums::Visibility;
use memoricai_core::error::{Error, Result};
use memoricai_core::model::Space;
use sqlx::postgres::PgRow;
use sqlx::Row;

fn map_space(row: &PgRow) -> Space {
    Space {
        id: row.get("id"),
        name: row.get("name"),
        description: row.get("description"),
        org_id: row.get("org_id"),
        owner_id: row.get("owner_id"),
        container_tag: row.get("container_tag"),
        entity_context: row.get("entity_context"),
        emoji: row.get("emoji"),
        visibility: Visibility::parse(&row.get::<String, _>("visibility")),
        is_experimental: row.get("is_experimental"),
        metadata: row.get("metadata"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn slugify(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "space".to_string()
    } else {
        s
    }
}

impl Db {
    pub async fn ensure_spaces(
        &self,
        org_id: &str,
        container_tags: &[String],
        owner_id: Option<&str>,
    ) -> Result<()> {
        if container_tags.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = container_tags
            .iter()
            .map(|_| memoricai_core::ids::project_id())
            .collect();
        sqlx::query(
            "INSERT INTO spaces (id, name, org_id, owner_id, container_tag)
             SELECT input.id, input.tag, $2, $3, input.tag
             FROM unnest($1::text[], $4::text[]) AS input(id, tag)
             ON CONFLICT (org_id, container_tag) DO NOTHING",
        )
        .bind(&ids)
        .bind(org_id)
        .bind(owner_id)
        .bind(container_tags)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Ensure a space exists for `container_tag`, creating it lazily on first write.
    pub async fn ensure_space(
        &self,
        org_id: &str,
        container_tag: &str,
        owner_id: Option<&str>,
    ) -> Result<Space> {
        let id = memoricai_core::ids::project_id();
        sqlx::query(
            "INSERT INTO spaces (id, name, org_id, owner_id, container_tag)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (org_id, container_tag) DO NOTHING",
        )
        .bind(&id)
        .bind(container_tag)
        .bind(org_id)
        .bind(owner_id)
        .bind(container_tag)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        self.get_space(org_id, container_tag)
            .await?
            .ok_or_else(|| Error::Internal("space vanished after ensure".into()))
    }

    pub async fn get_space(&self, org_id: &str, container_tag: &str) -> Result<Option<Space>> {
        let row = sqlx::query("SELECT * FROM spaces WHERE org_id = $1 AND container_tag = $2")
            .bind(org_id)
            .bind(container_tag)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_space))
    }

    pub async fn get_space_by_id(&self, org_id: &str, id: &str) -> Result<Option<Space>> {
        let row = sqlx::query("SELECT * FROM spaces WHERE org_id = $1 AND id = $2")
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_space))
    }

    /// All (org_id, container_tag) pairs across the deployment (for background jobs).
    pub async fn all_container_tags(&self) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT org_id, container_tag FROM spaces")
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|r| (r.get("org_id"), r.get("container_tag")))
            .collect())
    }

    pub async fn list_spaces(&self, org_id: &str) -> Result<Vec<Space>> {
        let rows = sqlx::query("SELECT * FROM spaces WHERE org_id = $1 ORDER BY created_at DESC")
            .bind(org_id)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(rows.iter().map(map_space).collect())
    }

    pub async fn create_space(
        &self,
        org_id: &str,
        name: &str,
        emoji: Option<&str>,
        owner_id: Option<&str>,
    ) -> Result<Space> {
        let slug = slugify(name);
        let base_tag = format!("mc_project_{slug}");
        // Ensure uniqueness within org.
        let tag = if self.get_space(org_id, &base_tag).await?.is_some() {
            format!("{base_tag}_{}", memoricai_core::ids::token(4))
        } else {
            base_tag
        };
        let id = memoricai_core::ids::project_id();
        let row = sqlx::query(
            "INSERT INTO spaces (id, name, org_id, owner_id, container_tag, emoji)
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
        )
        .bind(&id)
        .bind(name)
        .bind(org_id)
        .bind(owner_id)
        .bind(&tag)
        .bind(emoji)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(map_space(&row))
    }

    pub async fn update_space(
        &self,
        org_id: &str,
        container_tag: &str,
        name: Option<&str>,
        entity_context: Option<&str>,
    ) -> Result<Space> {
        let row = sqlx::query(
            "UPDATE spaces SET name = COALESCE($3, name),
             entity_context = COALESCE($4, entity_context), updated_at = now()
             WHERE org_id = $1 AND container_tag = $2 RETURNING *",
        )
        .bind(org_id)
        .bind(container_tag)
        .bind(name)
        .bind(entity_context)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.as_ref()
            .map(map_space)
            .ok_or_else(|| Error::NotFound(format!("space {container_tag}")))
    }

    /// Delete a project. `action` is "move" or "delete". Returns (docs_affected, memories_affected).
    pub async fn delete_space(
        &self,
        org_id: &str,
        id: &str,
        action: &str,
        target_container_tag: Option<&str>,
    ) -> Result<(u64, u64)> {
        let space = self
            .get_space_by_id(org_id, id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("project {id}")))?;
        let tag = &space.container_tag;

        if action == "move" && target_container_tag == Some(tag.as_str()) {
            return Err(Error::BadRequest(
                "target project must differ from source project".into(),
            ));
        }

        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let (docs, mems) = if action == "move" {
            let target = target_container_tag
                .ok_or_else(|| Error::BadRequest("targetProjectId required to move".into()))?;
            let d = sqlx::query(
                "UPDATE documents SET container_tags = CASE
                    WHEN $3 = ANY(array_remove(container_tags, $2)) THEN array_remove(container_tags, $2)
                    ELSE array_append(array_remove(container_tags, $2), $3)
                 END, updated_at = now()
                 WHERE org_id = $1 AND $2 = ANY(container_tags)",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            // Drop source memories that already have an equivalent copy under the target
            // tag (document shared by both projects) before re-tagging, else the move
            // duplicates them under the target.
            sqlx::query(
                "DELETE FROM memories m
                 WHERE m.org_id = $1 AND m.space_container_tag = $2
                   AND EXISTS (SELECT 1 FROM memories t
                               WHERE t.org_id = $1 AND t.space_container_tag = $3
                                 AND t.memory = m.memory
                                 AND t.document_id IS NOT DISTINCT FROM m.document_id)",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            let m = sqlx::query(
                "UPDATE memories SET space_container_tag = $3, updated_at = now()
                 WHERE org_id = $1 AND space_container_tag = $2",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            // Move shared chunk membership without copying or deleting the chunk/vector.
            sqlx::query(
                "INSERT INTO chunk_containers (chunk_id, container_tag)
                 SELECT membership.chunk_id, $3
                 FROM chunk_containers membership
                 JOIN chunks chunk ON chunk.id = membership.chunk_id
                 WHERE chunk.org_id = $1 AND membership.container_tag = $2 AND $2 <> $3
                 ON CONFLICT DO NOTHING",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "DELETE FROM chunk_containers membership
                 USING chunks chunk
                 WHERE membership.chunk_id = chunk.id
                   AND chunk.org_id = $1 AND membership.container_tag = $2 AND $2 <> $3",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "DELETE FROM profile_buckets source USING profile_buckets target
                 WHERE source.org_id=$1 AND source.container_tag=$2
                   AND target.org_id=$1 AND target.container_tag=$3 AND target.key=source.key",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "UPDATE profile_buckets SET container_tag=$3 WHERE org_id=$1 AND container_tag=$2",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "DELETE FROM profile_summaries source USING profile_summaries target
                 WHERE source.org_id=$1 AND source.container_tag=$2
                   AND target.org_id=$1 AND target.container_tag=$3
                   AND coalesce(target.bucket_key,'')=coalesce(source.bucket_key,'')",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query("UPDATE profile_summaries SET container_tag=$3 WHERE org_id=$1 AND container_tag=$2")
                .bind(org_id)
                .bind(tag)
                .bind(target)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            sqlx::query(
                "UPDATE connections SET container_tags = CASE
                   WHEN $3=ANY(array_remove(coalesce(container_tags,'{}'),$2))
                     THEN array_remove(coalesce(container_tags,'{}'),$2)
                   ELSE array_append(array_remove(coalesce(container_tags,'{}'),$2),$3)
                 END
                 WHERE org_id=$1 AND $2=ANY(coalesce(container_tags,'{}'))",
            )
            .bind(org_id)
            .bind(tag)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            (d.rows_affected(), m.rows_affected())
        } else if action == "delete" {
            let m =
                sqlx::query("DELETE FROM memories WHERE org_id = $1 AND space_container_tag = $2")
                    .bind(org_id)
                    .bind(tag)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            sqlx::query(
                "DELETE FROM chunk_containers membership
                 USING chunks chunk
                 WHERE membership.chunk_id = chunk.id
                   AND chunk.org_id = $1 AND membership.container_tag = $2",
            )
            .bind(org_id)
            .bind(tag)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            let removed = sqlx::query(
                "DELETE FROM documents WHERE org_id=$1 AND $2=ANY(container_tags)
                 AND cardinality(container_tags)=1",
            )
            .bind(org_id)
            .bind(tag)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            let retained = sqlx::query(
                "UPDATE documents SET container_tags=array_remove(container_tags,$2), updated_at=now()
                 WHERE org_id=$1 AND $2=ANY(container_tags)",
            )
            .bind(org_id)
            .bind(tag)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query("DELETE FROM profile_buckets WHERE org_id=$1 AND container_tag=$2")
                .bind(org_id)
                .bind(tag)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            sqlx::query("DELETE FROM profile_summaries WHERE org_id=$1 AND container_tag=$2")
                .bind(org_id)
                .bind(tag)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            sqlx::query(
                "UPDATE connections SET container_tags=array_remove(coalesce(container_tags,'{}'),$2)
                 WHERE org_id=$1 AND $2=ANY(coalesce(container_tags,'{}'))",
            )
            .bind(org_id)
            .bind(tag)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            (
                removed.rows_affected() + retained.rows_affected(),
                m.rows_affected(),
            )
        } else {
            return Err(Error::BadRequest("action must be move or delete".into()));
        };

        sqlx::query("DELETE FROM spaces WHERE org_id = $1 AND id = $2")
            .bind(org_id)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok((docs, mems))
    }
}
