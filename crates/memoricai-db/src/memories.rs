//! Memory repository: insert, vector search, version chains, edges, forgetting,
//! and profile-source queries.

use crate::{db_err, map_memory, pgvec, Db, MemoryHit};
use memoricai_core::enums::MemoryRelation;
use memoricai_core::error::{Error, Result};
use memoricai_core::model::Memory;
use sqlx::{Postgres, Row, Transaction};
use std::collections::HashMap;

/// A fully model-prepared memory waiting to be committed as part of a document index.
/// Relation/version decisions are intentionally made inside the database transaction so
/// chunks, memories, graph edges, bucket assignments, and the final document status move
/// together.
#[derive(Debug, Clone)]
pub struct ExtractedMemoryDraft {
    pub user_id: Option<String>,
    pub container_tag: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub is_static: bool,
    pub forget_after: Option<chrono::DateTime<chrono::Utc>>,
    pub event_date: Option<chrono::DateTime<chrono::Utc>>,
    pub bucket_key: Option<String>,
}

#[derive(Debug, Default)]
pub struct MemoryRelations {
    pub parents: Vec<(Memory, MemoryRelation)>,
    pub children: Vec<(Memory, MemoryRelation)>,
}

const UPDATE_THRESHOLD: f32 = 0.97;
const EXTEND_THRESHOLD: f32 = 0.85;

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

/// Insert model-prepared memories while holding the caller's index transaction.
///
/// Container-scoped advisory locks serialize version-chain decisions with other atomic
/// document replacements. All locks are taken in sorted order to avoid cross-container
/// deadlocks when a document belongs to more than one container.
pub(crate) async fn insert_extracted_memories(
    tx: &mut Transaction<'_, Postgres>,
    document_id: &str,
    org_id: &str,
    embedding_index_id: &str,
    embedding_dimension: usize,
    drafts: &[ExtractedMemoryDraft],
) -> Result<()> {
    let mut tags: Vec<&str> = drafts
        .iter()
        .map(|draft| draft.container_tag.as_str())
        .collect();
    tags.sort_unstable();
    tags.dedup();
    for tag in tags {
        let lock_key = format!("memoricai:memory-scope:{org_id}:{tag}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }

    struct PendingMemory {
        id: String,
        user_id: Option<String>,
        container_tag: String,
        content: String,
        embedding: Vec<f32>,
        version: i32,
        is_latest: bool,
        parent_memory_id: Option<String>,
        root_memory_id: Option<String>,
        relation: Option<MemoryRelation>,
        is_static: bool,
        forget_after: Option<chrono::DateTime<chrono::Utc>>,
        event_date: Option<chrono::DateTime<chrono::Utc>>,
        bucket_key: Option<String>,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    }

    fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
        let (dot, left_norm, right_norm) = left.iter().zip(right).fold(
            (0.0_f64, 0.0_f64, 0.0_f64),
            |(dot, left_norm, right_norm), (left, right)| {
                let left = f64::from(*left);
                let right = f64::from(*right);
                (
                    dot + left * right,
                    left_norm + left * left,
                    right_norm + right * right,
                )
            },
        );
        (dot / (left_norm.sqrt() * right_norm.sqrt())) as f32
    }

    let mut pending: Vec<PendingMemory> = Vec::with_capacity(drafts.len());
    let mut edges: Vec<(String, String, MemoryRelation)> = Vec::new();
    for draft in drafts {
        let new_id = memoricai_core::ids::memory_id();
        let (stored_vector, query_vector) =
            crate::embeddings::vector_search_operands("me.embedding", embedding_dimension)?;
        let index_id = crate::embeddings::sql_text_literal(embedding_index_id);
        let neighbor_sql = format!(
            r#"SELECT m.*, 1 - ({stored_vector} <=> {query_vector}) AS similarity
               FROM memories m
               JOIN memory_embeddings me ON me.memory_id=m.id AND me.index_id={index_id}
               WHERE m.org_id = $2 AND m.space_container_tag = $3
                 AND m.is_latest AND NOT m.is_forgotten
                 AND (m.document_id IS NULL OR m.document_id = $4 OR EXISTS (
                       SELECT 1 FROM documents d
                       WHERE d.id = m.document_id AND d.status = 'done'))
               ORDER BY {stored_vector} <=> {query_vector}
               LIMIT 1
               FOR UPDATE OF m"#,
        );
        let neighbor = sqlx::query(&neighbor_sql)
            .bind(pgvec(&draft.embedding))
            .bind(org_id)
            .bind(&draft.container_tag)
            .bind(document_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_err)?;

        let database_top = neighbor
            .as_ref()
            .map(|row| (map_memory(row), row.get::<f64, _>("similarity") as f32));
        let local_top = pending
            .iter()
            .enumerate()
            .filter(|(_, memory)| memory.is_latest && memory.container_tag == draft.container_tag)
            .map(|(position, memory)| {
                (
                    position,
                    cosine_similarity(&draft.embedding, &memory.embedding),
                )
            })
            .max_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let database_similarity = database_top
            .as_ref()
            .map(|(_, similarity)| *similarity)
            .unwrap_or(f32::NEG_INFINITY);
        let use_local = local_top
            .as_ref()
            .is_some_and(|(_, similarity)| *similarity > database_similarity);
        let top_similarity = local_top
            .filter(|_| use_local)
            .map(|(_, similarity)| similarity)
            .unwrap_or(database_similarity);

        let now = chrono::Utc::now();
        let (version, parent, root, relation, source_id, supersede_database) = if top_similarity
            >= UPDATE_THRESHOLD
        {
            if use_local {
                let position = local_top
                    .map(|(position, _)| position)
                    .ok_or_else(|| Error::Internal("local memory candidate vanished".into()))?;
                let previous = &mut pending[position];
                previous.is_latest = false;
                previous.updated_at = now;
                let root = previous
                    .root_memory_id
                    .clone()
                    .unwrap_or_else(|| previous.id.clone());
                (
                    previous.version + 1,
                    Some(previous.id.clone()),
                    Some(root),
                    Some(MemoryRelation::Updates),
                    Some(previous.id.clone()),
                    None,
                )
            } else {
                let memory = &database_top
                    .as_ref()
                    .ok_or_else(|| Error::Internal("database memory candidate vanished".into()))?
                    .0;
                let root = memory
                    .root_memory_id
                    .clone()
                    .unwrap_or_else(|| memory.id.clone());
                (
                    memory.version + 1,
                    Some(memory.id.clone()),
                    Some(root),
                    Some(MemoryRelation::Updates),
                    Some(memory.id.clone()),
                    Some(memory.id.clone()),
                )
            }
        } else if top_similarity >= EXTEND_THRESHOLD {
            let source_id = if use_local {
                let position = local_top
                    .map(|(position, _)| position)
                    .ok_or_else(|| Error::Internal("local memory candidate vanished".into()))?;
                Some(pending[position].id.clone())
            } else {
                database_top.as_ref().map(|(memory, _)| memory.id.clone())
            };
            (
                1,
                None,
                None,
                Some(MemoryRelation::Extends),
                source_id,
                None,
            )
        } else {
            (1, None, None, None, None, None)
        };

        if let Some(previous_id) = supersede_database.as_deref() {
            let updated = sqlx::query(
                "UPDATE memories SET is_latest=false, updated_at=now()
                 WHERE id=$1 AND is_latest",
            )
            .bind(previous_id)
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
            if updated.rows_affected() == 0 {
                return Err(Error::Conflict("memory was updated concurrently".into()));
            }
        }
        if let Some(source_id) = source_id {
            let edge_relation = relation.ok_or_else(|| {
                Error::Internal("memory graph edge is missing its relation".into())
            })?;
            edges.push((source_id, new_id.clone(), edge_relation));
        }
        pending.push(PendingMemory {
            id: new_id,
            user_id: draft.user_id.clone(),
            container_tag: draft.container_tag.clone(),
            content: draft.content.clone(),
            embedding: draft.embedding.clone(),
            version,
            is_latest: true,
            parent_memory_id: parent,
            root_memory_id: root,
            relation,
            is_static: draft.is_static,
            forget_after: draft.forget_after,
            event_date: draft.event_date,
            bucket_key: draft.bucket_key.clone(),
            created_at: now,
            updated_at: now,
        });
    }

    if pending.is_empty() {
        return Ok(());
    }

    let records: Vec<serde_json::Value> = pending
        .iter()
        .map(|memory| {
            serde_json::json!({
                "id": memory.id,
                "user_id": memory.user_id,
                "memory": memory.content,
                "space_container_tag": memory.container_tag,
                "version": memory.version,
                "is_latest": memory.is_latest,
                "parent_memory_id": memory.parent_memory_id,
                "root_memory_id": memory.root_memory_id,
                "relation": memory.relation.map(|relation| relation.as_str()),
                "is_static": memory.is_static,
                "forget_after": memory.forget_after,
                "event_date": memory.event_date,
                "bucket_key": memory.bucket_key,
                "created_at": memory.created_at,
                "updated_at": memory.updated_at,
            })
        })
        .collect();
    sqlx::query(
        r#"INSERT INTO memories
           (id, document_id, org_id, user_id, memory, space_container_tag,
            version, is_latest, parent_memory_id, root_memory_id, relation, source_count,
            is_static, is_inference, is_forgotten, forget_after, event_date, metadata,
            bucket_key, created_at, updated_at)
           SELECT input.id, $2, $3, input.user_id, input.memory,
                  input.space_container_tag, input.version, input.is_latest,
                  input.parent_memory_id, input.root_memory_id, input.relation, 1,
                  input.is_static, false, false, input.forget_after, input.event_date,
                  '{}'::jsonb, input.bucket_key, input.created_at, input.updated_at
           FROM jsonb_to_recordset($1::jsonb) AS input(
                id text, user_id text, memory text, space_container_tag text,
                version int4, is_latest boolean, parent_memory_id text,
                root_memory_id text, relation text, is_static boolean,
                forget_after timestamptz, event_date timestamptz, bucket_key text,
                created_at timestamptz, updated_at timestamptz)"#,
    )
    .bind(serde_json::Value::Array(records))
    .bind(document_id)
    .bind(org_id)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    let memory_ids: Vec<&str> = pending.iter().map(|memory| memory.id.as_str()).collect();
    let vectors: Vec<String> = pending
        .iter()
        .map(|memory| pgvec(&memory.embedding))
        .collect();
    sqlx::query(
        "INSERT INTO memory_embeddings (index_id, memory_id, embedding)
         SELECT $1, input.id, input.embedding::vector
         FROM unnest($2::text[], $3::text[]) AS input(id, embedding)",
    )
    .bind(embedding_index_id)
    .bind(&memory_ids)
    .bind(&vectors)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    if !edges.is_empty() {
        let sources: Vec<&str> = edges.iter().map(|(source, _, _)| source.as_str()).collect();
        let targets: Vec<&str> = edges.iter().map(|(_, target, _)| target.as_str()).collect();
        let relations: Vec<&str> = edges
            .iter()
            .map(|(_, _, relation)| relation.as_str())
            .collect();
        sqlx::query(
            "INSERT INTO memory_relations (source_memory_id, target_memory_id, relation)
             SELECT * FROM unnest($1::text[], $2::text[], $3::text[])",
        )
        .bind(&sources)
        .bind(&targets)
        .bind(&relations)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }
    Ok(())
}

impl Db {
    pub async fn insert_memories(
        &self,
        memories: &[Memory],
        embedding_index_id: &str,
        embeddings: &[Vec<f32>],
    ) -> Result<()> {
        if memories.len() != embeddings.len() {
            return Err(Error::Internal(
                "memory and embedding batches are misaligned".into(),
            ));
        }
        if memories.is_empty() {
            return Ok(());
        }
        let org_id = &memories[0].org_id;
        if memories.iter().any(|memory| &memory.org_id != org_id) {
            return Err(Error::Internal(
                "memory batch spans multiple organizations".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let dimension =
            crate::embeddings::embedding_index_dimension(&mut tx, embedding_index_id, Some(org_id))
                .await?;
        for embedding in embeddings {
            crate::embeddings::validate_stored_embedding(embedding, dimension)?;
        }
        let records: Vec<serde_json::Value> = memories
            .iter()
            .map(|memory| {
                serde_json::json!({
                    "id": memory.id,
                    "custom_id": memory.custom_id,
                    "document_id": memory.document_id,
                    "org_id": memory.org_id,
                    "user_id": memory.user_id,
                    "memory": memory.memory,
                    "summary": memory.summary,
                    "mem_type": memory.mem_type,
                    "space_container_tag": memory.space_container_tag,
                    "version": memory.version,
                    "is_latest": memory.is_latest,
                    "parent_memory_id": memory.parent_memory_id,
                    "root_memory_id": memory.root_memory_id,
                    "relation": memory.relation.map(|relation| relation.as_str()),
                    "source_count": memory.source_count,
                    "is_static": memory.is_static,
                    "is_inference": memory.is_inference,
                    "review_status": memory.review_status,
                    "is_forgotten": memory.is_forgotten,
                    "forget_reason": memory.forget_reason,
                    "forget_after": memory.forget_after,
                    "forget_batch_id": memory.forget_batch_id,
                    "event_date": memory.event_date,
                    "metadata": memory.metadata,
                    "created_at": memory.created_at,
                    "updated_at": memory.updated_at,
                })
            })
            .collect();
        sqlx::query(
            r#"INSERT INTO memories
               (id, custom_id, document_id, org_id, user_id, memory, summary, mem_type,
                space_container_tag, version, is_latest, parent_memory_id, root_memory_id,
                relation, source_count, is_static, is_inference, review_status, is_forgotten,
                forget_reason, forget_after, forget_batch_id, event_date, metadata,
                created_at, updated_at)
               SELECT input.id, input.custom_id, input.document_id, input.org_id, input.user_id,
                      input.memory, input.summary, input.mem_type, input.space_container_tag,
                      input.version, input.is_latest, input.parent_memory_id, input.root_memory_id,
                      input.relation, input.source_count, input.is_static, input.is_inference,
                      input.review_status, input.is_forgotten, input.forget_reason,
                      input.forget_after, input.forget_batch_id, input.event_date, input.metadata,
                      input.created_at, input.updated_at
               FROM jsonb_to_recordset($1::jsonb) AS input(
                    id text, custom_id text, document_id text, org_id text, user_id text,
                    memory text, summary text, mem_type text, space_container_tag text,
                    version int4, is_latest boolean, parent_memory_id text, root_memory_id text,
                    relation text, source_count int4, is_static boolean, is_inference boolean,
                    review_status text, is_forgotten boolean, forget_reason text,
                    forget_after timestamptz, forget_batch_id text, event_date timestamptz,
                    metadata jsonb, created_at timestamptz, updated_at timestamptz)"#,
        )
        .bind(serde_json::Value::Array(records))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        let ids: Vec<&str> = memories.iter().map(|memory| memory.id.as_str()).collect();
        let vectors: Vec<String> = embeddings
            .iter()
            .map(|embedding| pgvec(embedding))
            .collect();
        sqlx::query(
            "INSERT INTO memory_embeddings (index_id, memory_id, embedding)
             SELECT $1, input.id, input.embedding::vector
             FROM unnest($2::text[], $3::text[]) AS input(id, embedding)",
        )
        .bind(embedding_index_id)
        .bind(&ids)
        .bind(&vectors)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn insert_memory(
        &self,
        mem: &Memory,
        embedding_index_id: &str,
        embedding: &[f32],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let dimension = crate::embeddings::embedding_index_dimension(
            &mut tx,
            embedding_index_id,
            Some(&mem.org_id),
        )
        .await?;
        crate::embeddings::validate_stored_embedding(embedding, dimension)?;
        sqlx::query(
            r#"INSERT INTO memories
               (id, custom_id, document_id, org_id, user_id, memory, summary, mem_type,
                space_container_tag, version, is_latest, parent_memory_id, root_memory_id,
                relation, source_count, is_static, is_inference, review_status, is_forgotten,
                forget_reason, forget_after, forget_batch_id, event_date, metadata, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,
                       $18,$19,$20,$21,$22,$23,$24,$25,$26)"#,
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
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO memory_embeddings (index_id, memory_id, embedding)
             VALUES ($1,$2,$3::vector)",
        )
        .bind(embedding_index_id)
        .bind(&mem.id)
        .bind(pgvec(embedding))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    /// Atomically retire one latest memory, append its successor, and add the graph edge.
    pub async fn replace_latest_memory(
        &self,
        previous_id: &str,
        mem: &Memory,
        embedding_index_id: &str,
        embedding: &[f32],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let dimension = crate::embeddings::embedding_index_dimension(
            &mut tx,
            embedding_index_id,
            Some(&mem.org_id),
        )
        .await?;
        crate::embeddings::validate_stored_embedding(embedding, dimension)?;
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
                space_container_tag, version, is_latest, parent_memory_id, root_memory_id,
                relation, source_count, is_static, is_inference, review_status, is_forgotten,
                forget_reason, forget_after, forget_batch_id, event_date, metadata, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,
                       $18,$19,$20,$21,$22,$23,$24,$25,$26)"#,
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
            "INSERT INTO memory_embeddings (index_id, memory_id, embedding)
             VALUES ($1,$2,$3::vector)",
        )
        .bind(embedding_index_id)
        .bind(&mem.id)
        .bind(pgvec(embedding))
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

    #[allow(clippy::too_many_arguments)]
    pub async fn search_memories(
        &self,
        org_id: &str,
        embedding_index_id: &str,
        embedding_dimension: usize,
        tag: Option<&str>,
        qvec: &[f32],
        k: i64,
        threshold: f32,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        let (stored_vector, query_vector) =
            crate::embeddings::vector_search_operands("me.embedding", embedding_dimension)?;
        let index_id = crate::embeddings::sql_text_literal(embedding_index_id);
        let sql = format!(
            r#"SELECT m.*, 1 - ({stored_vector} <=> {query_vector}) AS similarity
               FROM memories m
               JOIN memory_embeddings me ON me.memory_id=m.id AND me.index_id={index_id}
               WHERE m.org_id = $2
                 AND ($3::text IS NULL OR m.space_container_tag = $3)
                 AND m.is_latest
                 -- Only surface memories whose source document is fully indexed, so a
                 -- partially-rebuilt (failed / in-progress) reindex is never searchable.
                 AND (m.document_id IS NULL
                      OR EXISTS (SELECT 1 FROM documents d
                                 WHERE d.id = m.document_id AND d.status = 'done'))
                 AND ($6 OR NOT m.is_forgotten)
                 AND 1 - ({stored_vector} <=> {query_vector}) >= $4
               ORDER BY {stored_vector} <=> {query_vector}
               LIMIT $5"#,
        );
        let rows = sqlx::query(&sql)
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
    #[allow(clippy::too_many_arguments)]
    pub async fn neighbor_memories(
        &self,
        org_id: &str,
        embedding_index_id: &str,
        embedding_dimension: usize,
        tag: &str,
        qvec: &[f32],
        k: i64,
        exclude_id: &str,
    ) -> Result<Vec<MemoryHit>> {
        let (stored_vector, query_vector) =
            crate::embeddings::vector_search_operands("me.embedding", embedding_dimension)?;
        let index_id = crate::embeddings::sql_text_literal(embedding_index_id);
        let sql = format!(
            r#"SELECT m.*, 1 - ({stored_vector} <=> {query_vector}) AS similarity
             FROM memories m
             JOIN memory_embeddings me ON me.memory_id=m.id AND me.index_id={index_id}
             WHERE m.org_id = $2 AND m.space_container_tag = $3
                 AND m.is_latest AND NOT m.is_forgotten
                 AND m.id <> $4
                 AND (m.document_id IS NULL OR EXISTS (
                      SELECT 1 FROM documents d
                      WHERE d.id = m.document_id AND d.status = 'done'))
               ORDER BY {stored_vector} <=> {query_vector}
               LIMIT $5"#,
        );
        let rows = sqlx::query(&sql)
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

    /// Fetch both directions of the relation graph for a page of memories in
    /// one round trip, capped to the same eight entries per direction as the
    /// single-memory methods.
    pub async fn memory_relations_for_ids(
        &self,
        ids: &[String],
    ) -> Result<HashMap<String, MemoryRelations>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "WITH edges AS (
               SELECT target_memory_id AS requested_id, source_memory_id AS related_id,
                      relation, 'parent'::text AS direction
               FROM memory_relations WHERE target_memory_id=ANY($1)
               UNION ALL
               SELECT source_memory_id AS requested_id, target_memory_id AS related_id,
                      relation, 'child'::text AS direction
               FROM memory_relations WHERE source_memory_id=ANY($1)
             ), ranked AS (
               SELECT *, row_number() OVER (
                   PARTITION BY requested_id, direction ORDER BY related_id) AS position
               FROM edges
             )
             SELECT ranked.requested_id, ranked.direction,
                    ranked.relation AS edge_relation, m.*
             FROM ranked JOIN memories m ON m.id=ranked.related_id
             WHERE ranked.position <= 8
             ORDER BY ranked.requested_id, ranked.direction, ranked.position",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let mut relations: HashMap<String, MemoryRelations> = HashMap::new();
        for row in &rows {
            let requested_id: String = row.get("requested_id");
            let relation = MemoryRelation::parse(&row.get::<String, _>("edge_relation"));
            let entry = relations.entry(requested_id).or_default();
            let value = (map_memory(row), relation);
            if row.get::<String, _>("direction") == "parent" {
                entry.parents.push(value);
            } else {
                entry.children.push(value);
            }
        }
        Ok(relations)
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
             AND (document_id IS NULL OR EXISTS (
                  SELECT 1 FROM documents d
                  WHERE d.id = memories.document_id AND d.status = 'done'))
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
             AND (document_id IS NULL OR EXISTS (
                  SELECT 1 FROM documents d
                  WHERE d.id = memories.document_id AND d.status = 'done'))
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

    /// Inferred (derived) memories awaiting review, ordered by source strength.
    pub async fn list_inferred(&self, org_id: &str, container_tag: &str) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT * FROM memories WHERE org_id = $1 AND space_container_tag = $2
             AND is_inference AND review_status IS NULL AND is_latest AND NOT is_forgotten
             AND (document_id IS NULL OR EXISTS (
                  SELECT 1 FROM documents d
                  WHERE d.id = memories.document_id AND d.status = 'done'))
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
