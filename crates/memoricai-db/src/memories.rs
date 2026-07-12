//! Memory repository: insert, vector search, version chains, edges, forgetting,
//! and profile-source queries.

use crate::{db_err, map_memory, pgvec, Db, MemoryHit};
use memoricai_core::enums::MemoryRelation;
use memoricai_core::error::{Error, Result};
use memoricai_core::model::Memory;
use sqlx::{Postgres, Row, Transaction};
use std::collections::HashMap;

pub(crate) async fn prepare_memories_for_document_deletion(
    tx: &mut Transaction<'_, Postgres>,
    document_ids: &[String],
) -> Result<()> {
    if document_ids.is_empty() {
        return Ok(());
    }
    let predecessors: Vec<String> = sqlx::query_scalar(
        "WITH RECURSIVE ancestors AS (
           SELECT id, parent_memory_id, document_id,
                  coalesce(root_memory_id,id) AS chain_root, 0 AS depth
           FROM memories WHERE document_id=ANY($1) AND is_latest
           UNION ALL
           SELECT parent.id, parent.parent_memory_id, parent.document_id,
                  child.chain_root, child.depth+1
           FROM memories parent
           JOIN ancestors child ON parent.id=child.parent_memory_id
           WHERE child.depth < 1000
         )
         SELECT DISTINCT ON (chain_root) id
         FROM ancestors
         WHERE depth > 0
           AND (document_id IS NULL OR NOT (document_id=ANY($1)))
         ORDER BY chain_root, depth",
    )
    .bind(document_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(db_err)?;
    // Preserve historical versions that may be ancestors of a memory owned by
    // a surviving document, but detach their deleted source document.
    sqlx::query(
        "UPDATE memories SET document_id=NULL
         WHERE document_id=ANY($1) AND NOT is_latest",
    )
    .bind(document_ids)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;
    sqlx::query("DELETE FROM memories WHERE document_id=ANY($1)")
        .bind(document_ids)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    if !predecessors.is_empty() {
        sqlx::query("UPDATE memories SET is_latest=true, updated_at=now() WHERE id=ANY($1)")
            .bind(&predecessors)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

impl Db {
    pub async fn insert_memory(&self, mem: &Memory, embedding: &[f32]) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO memories
               (id, custom_id, document_id, org_id, user_id, memory, summary, mem_type,
                space_container_tag, embedding, version, is_latest, parent_memory_id, root_memory_id,
                relation, source_count, is_static, is_inference, review_status, is_forgotten,
                forget_reason, forget_after, forget_batch_id, event_date, metadata, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::vector,$11,$12,$13,$14,$15,$16,$17,$18,
                       $19,$20,$21,$22,$23,$24,$25,$26,$27)"#,
        )
        .bind(&mem.id)
        .bind(&mem.custom_id)
        .bind(&mem.document_id)
        .bind(&mem.org_id)
        .bind(&mem.user_id)
        .bind(&mem.memory)
        .bind(&mem.summary)
        .bind(&mem.mem_type)
        .bind(&mem.space_container_tag)
        .bind(pgvec(embedding))
        .bind(mem.version)
        .bind(mem.is_latest)
        .bind(&mem.parent_memory_id)
        .bind(&mem.root_memory_id)
        .bind(mem.relation.map(|r| r.as_str()))
        .bind(mem.source_count)
        .bind(mem.is_static)
        .bind(mem.is_inference)
        .bind(&mem.review_status)
        .bind(mem.is_forgotten)
        .bind(&mem.forget_reason)
        .bind(mem.forget_after)
        .bind(&mem.forget_batch_id)
        .bind(mem.event_date)
        .bind(&mem.metadata)
        .bind(mem.created_at)
        .bind(mem.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Atomically retire one latest memory, append its successor, and add the graph edge.
    pub async fn replace_latest_memory(
        &self,
        previous_id: &str,
        mem: &Memory,
        embedding: &[f32],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let previous = sqlx::query(
            "SELECT org_id, space_container_tag, is_latest FROM memories WHERE id = $1 FOR UPDATE",
        )
        .bind(previous_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?
        .ok_or_else(|| Error::NotFound(format!("memory {previous_id}")))?;
        if previous.get::<String, _>("org_id") != mem.org_id
            || previous.get::<String, _>("space_container_tag") != mem.space_container_tag
        {
            return Err(Error::Forbidden("memory version scope mismatch".into()));
        }
        if !previous.get::<bool, _>("is_latest") {
            return Err(Error::Conflict("memory was updated concurrently".into()));
        }
        sqlx::query("UPDATE memories SET is_latest = false, updated_at = now() WHERE id = $1")
            .bind(previous_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query(
            r#"INSERT INTO memories
               (id, custom_id, document_id, org_id, user_id, memory, summary, mem_type,
                space_container_tag, embedding, version, is_latest, parent_memory_id, root_memory_id,
                relation, source_count, is_static, is_inference, review_status, is_forgotten,
                forget_reason, forget_after, forget_batch_id, event_date, metadata, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::vector,$11,$12,$13,$14,$15,$16,$17,$18,
                       $19,$20,$21,$22,$23,$24,$25,$26,$27)"#,
        )
        .bind(&mem.id)
        .bind(&mem.custom_id)
        .bind(&mem.document_id)
        .bind(&mem.org_id)
        .bind(&mem.user_id)
        .bind(&mem.memory)
        .bind(&mem.summary)
        .bind(&mem.mem_type)
        .bind(&mem.space_container_tag)
        .bind(pgvec(embedding))
        .bind(mem.version)
        .bind(mem.is_latest)
        .bind(&mem.parent_memory_id)
        .bind(&mem.root_memory_id)
        .bind(mem.relation.map(|relation| relation.as_str()))
        .bind(mem.source_count)
        .bind(mem.is_static)
        .bind(mem.is_inference)
        .bind(&mem.review_status)
        .bind(mem.is_forgotten)
        .bind(&mem.forget_reason)
        .bind(mem.forget_after)
        .bind(&mem.forget_batch_id)
        .bind(mem.event_date)
        .bind(&mem.metadata)
        .bind(mem.created_at)
        .bind(mem.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO memory_relations (source_memory_id, target_memory_id, relation)
             VALUES ($1,$2,'updates')",
        )
        .bind(previous_id)
        .bind(&mem.id)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn get_memory(&self, org_id: &str, id: &str) -> Result<Memory> {
        let row = sqlx::query("SELECT * FROM memories WHERE org_id = $1 AND id = $2")
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        row.as_ref()
            .map(map_memory)
            .ok_or_else(|| Error::NotFound(format!("memory {id}")))
    }

    pub async fn search_memories(
        &self,
        org_id: &str,
        tag: Option<&str>,
        qvec: &[f32],
        k: i64,
        threshold: f32,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        let rows = sqlx::query(
            r#"SELECT *, 1 - (embedding <=> $1::vector) AS similarity
               FROM memories
               WHERE org_id = $2
                 AND ($3::text IS NULL OR space_container_tag = $3)
                 AND is_latest
                 AND ($6 OR NOT is_forgotten)
                 AND embedding IS NOT NULL
                 AND 1 - (embedding <=> $1::vector) >= $4
               ORDER BY embedding <=> $1::vector
               LIMIT $5"#,
        )
        .bind(pgvec(qvec))
        .bind(org_id)
        .bind(tag)
        .bind(threshold as f64)
        .bind(k)
        .bind(include_forgotten)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|row| MemoryHit {
                memory: map_memory(row),
                similarity: row.get::<f64, _>("similarity") as f32,
            })
            .collect())
    }

    /// Nearest neighbors for relation inference (no threshold, excludes `exclude_id`).
    pub async fn neighbor_memories(
        &self,
        org_id: &str,
        tag: &str,
        qvec: &[f32],
        k: i64,
        exclude_id: &str,
    ) -> Result<Vec<MemoryHit>> {
        let rows = sqlx::query(
            r#"SELECT *, 1 - (embedding <=> $1::vector) AS similarity
               FROM memories
               WHERE org_id = $2 AND space_container_tag = $3 AND is_latest AND NOT is_forgotten
                 AND id <> $4 AND embedding IS NOT NULL
               ORDER BY embedding <=> $1::vector
               LIMIT $5"#,
        )
        .bind(pgvec(qvec))
        .bind(org_id)
        .bind(tag)
        .bind(exclude_id)
        .bind(k)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|row| MemoryHit {
                memory: map_memory(row),
                similarity: row.get::<f64, _>("similarity") as f32,
            })
            .collect())
    }

    pub async fn forget_memory_by_id(
        &self,
        org_id: &str,
        id: &str,
        reason: Option<&str>,
        batch: Option<&str>,
    ) -> Result<Option<Memory>> {
        let row = sqlx::query(
            "UPDATE memories SET is_forgotten = true, forget_reason = COALESCE($3, forget_reason),
             forget_batch_id = COALESCE($4, forget_batch_id), updated_at = now()
             WHERE org_id = $1 AND id = $2 RETURNING *",
        )
        .bind(org_id)
        .bind(id)
        .bind(reason)
        .bind(batch)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.as_ref().map(map_memory))
    }

    pub async fn forget_memory_by_content(
        &self,
        org_id: &str,
        tag: &str,
        content: &str,
        reason: Option<&str>,
    ) -> Result<Option<Memory>> {
        let row = sqlx::query(
            "UPDATE memories SET is_forgotten = true, forget_reason = COALESCE($4, forget_reason),
             updated_at = now()
             WHERE id = (SELECT id FROM memories WHERE org_id = $1 AND space_container_tag = $2
                         AND memory = $3 AND is_latest AND NOT is_forgotten LIMIT 1)
             RETURNING *",
        )
        .bind(org_id)
        .bind(tag)
        .bind(content)
        .bind(reason)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.as_ref().map(map_memory))
    }

    pub async fn insert_edge(
        &self,
        source: &str,
        target: &str,
        relation: MemoryRelation,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO memory_relations (source_memory_id, target_memory_id, relation)
             VALUES ($1,$2,$3) ON CONFLICT (source_memory_id, target_memory_id)
             DO UPDATE SET relation = EXCLUDED.relation",
        )
        .bind(source)
        .bind(target)
        .bind(relation.as_str())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Memories that `id` was derived/updated from (edge source -> id).
    pub async fn memory_parents(&self, id: &str) -> Result<Vec<(Memory, MemoryRelation)>> {
        let rows = sqlx::query(
            "SELECT m.*, r.relation AS edge_relation FROM memory_relations r
             JOIN memories m ON m.id = r.source_memory_id WHERE r.target_memory_id = $1 LIMIT 8",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|row| {
                let rel = MemoryRelation::parse(&row.get::<String, _>("edge_relation"));
                (map_memory(row), rel)
            })
            .collect())
    }

    /// Memories derived/updated from `id` (edge id -> target).
    pub async fn memory_children(&self, id: &str) -> Result<Vec<(Memory, MemoryRelation)>> {
        let rows = sqlx::query(
            "SELECT m.*, r.relation AS edge_relation FROM memory_relations r
             JOIN memories m ON m.id = r.target_memory_id WHERE r.source_memory_id = $1 LIMIT 8",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|row| {
                let rel = MemoryRelation::parse(&row.get::<String, _>("edge_relation"));
                (map_memory(row), rel)
            })
            .collect())
    }

    pub async fn static_memories(
        &self,
        org_id: &str,
        tag: &str,
        limit: i64,
    ) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT * FROM memories WHERE org_id = $1 AND space_container_tag = $2
             AND is_static AND is_latest AND NOT is_forgotten
             ORDER BY updated_at DESC LIMIT $3",
        )
        .bind(org_id)
        .bind(tag)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_memory).collect())
    }

    pub async fn recent_memories(
        &self,
        org_id: &str,
        tag: &str,
        limit: i64,
    ) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT * FROM memories WHERE org_id = $1 AND space_container_tag = $2
             AND NOT is_static AND is_latest AND NOT is_forgotten
             ORDER BY created_at DESC LIMIT $3",
        )
        .bind(org_id)
        .bind(tag)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_memory).collect())
    }

    /// Latest memories for each of `doc_ids`, grouped by document id.
    pub async fn memories_for_documents(
        &self,
        doc_ids: &[String],
    ) -> Result<HashMap<String, Vec<Memory>>> {
        let rows = sqlx::query(
            "SELECT * FROM memories WHERE document_id = ANY($1) AND is_latest
             ORDER BY document_id, created_at ASC",
        )
        .bind(doc_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let mut map: HashMap<String, Vec<Memory>> = HashMap::new();
        for row in &rows {
            let doc_id: String = row.get("document_id");
            map.entry(doc_id).or_default().push(map_memory(row));
        }
        Ok(map)
    }

    pub async fn delete_memories_for_document(&self, document_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        prepare_memories_for_document_deletion(&mut tx, &[document_id.to_string()]).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    /// Inferred (derived) memories awaiting review, ordered by source strength.
    pub async fn list_inferred(&self, org_id: &str, container_tag: &str) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT * FROM memories WHERE org_id = $1 AND space_container_tag = $2
             AND is_inference AND review_status IS NULL AND is_latest AND NOT is_forgotten
             ORDER BY source_count DESC, created_at DESC LIMIT 50",
        )
        .bind(org_id)
        .bind(container_tag)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_memory).collect())
    }

    /// Approve / decline / undo an inferred memory. Returns the updated row.
    pub async fn review_inferred(
        &self,
        org_id: &str,
        container_tag: &str,
        memory_id: &str,
        action: &str,
    ) -> Result<Option<Memory>> {
        let sql = match action {
            "approve" => {
                "UPDATE memories SET review_status='approved', is_forgotten=false, updated_at=now()
                 WHERE org_id=$1 AND space_container_tag=$2 AND id=$3 AND is_inference RETURNING *"
            }
            "decline" => {
                "UPDATE memories SET review_status='declined', is_forgotten=true,
                 forget_reason='declined', updated_at=now()
                 WHERE org_id=$1 AND space_container_tag=$2 AND id=$3 AND is_inference RETURNING *"
            }
            _ => {
                // undo
                "UPDATE memories SET review_status=NULL, is_forgotten=false, forget_reason=NULL, updated_at=now()
                 WHERE org_id=$1 AND space_container_tag=$2 AND id=$3 AND is_inference RETURNING *"
            }
        };
        let row = sqlx::query(sql)
            .bind(org_id)
            .bind(container_tag)
            .bind(memory_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_memory))
    }

    /// Mark expired (`forget_after < now`) memories forgotten. Returns count.
    pub async fn sweep_forgotten(&self) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE memories SET is_forgotten = true, forget_reason = 'expired', updated_at = now()
             WHERE forget_after IS NOT NULL AND forget_after < now() AND NOT is_forgotten",
        )
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(r.rows_affected())
    }
}
