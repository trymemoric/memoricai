//! Versioned embedding-index registry and durable background backfill jobs.

use crate::{db_err, pgvec, Db};
use memoricai_core::error::{Error, Result};
use sqlx::{Postgres, Row, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIndex {
    pub id: String,
    pub org_id: String,
    pub embedding_model_id: String,
    pub model_version: String,
    pub provider: String,
    pub dimension: usize,
}

#[derive(Debug, Clone, Default)]
pub struct EmbeddingBackfillBatch {
    pub memories: Vec<(String, String)>,
    pub chunks: Vec<(String, String)>,
}

fn validate_identity(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(Error::Model(format!(
            "embedding {field} must contain 1..=255 printable bytes"
        )));
    }
    Ok(())
}

pub(crate) async fn embedding_index_dimension(
    tx: &mut Transaction<'_, Postgres>,
    index_id: &str,
    org_id: Option<&str>,
) -> Result<usize> {
    let dimension: Option<i32> = sqlx::query_scalar(
        "SELECT dimension FROM embedding_indexes
         WHERE id=$1 AND ($2::text IS NULL OR org_id=$2)",
    )
    .bind(index_id)
    .bind(org_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;
    dimension
        .map(|value| value as usize)
        .ok_or_else(|| Error::Model("embedding index does not belong to this organization".into()))
}

pub(crate) fn validate_stored_embedding(embedding: &[f32], dimension: usize) -> Result<()> {
    if embedding.len() != dimension {
        return Err(Error::Model(format!(
            "embedding dimension {} does not match index dimension {dimension}",
            embedding.len()
        )));
    }
    if embedding.is_empty()
        || embedding.iter().any(|value| !value.is_finite())
        || embedding.iter().map(|value| value * value).sum::<f32>() <= f32::EPSILON
    {
        return Err(Error::Model(
            "embedding contains invalid numeric values".into(),
        ));
    }
    Ok(())
}

async fn queue_missing_backfill(
    tx: &mut Transaction<'_, Postgres>,
    org_id: &str,
    index_id: &str,
) -> Result<bool> {
    let missing: bool = sqlx::query_scalar(
        "SELECT
           EXISTS (
             SELECT 1 FROM memories m
             WHERE m.org_id=$1 AND NOT EXISTS (
               SELECT 1 FROM memory_embeddings e
               WHERE e.index_id=$2 AND e.memory_id=m.id))
           OR EXISTS (
             SELECT 1 FROM chunks c
             WHERE c.org_id=$1 AND NOT EXISTS (
               SELECT 1 FROM chunk_embeddings e
               WHERE e.index_id=$2 AND e.chunk_id=c.id))",
    )
    .bind(org_id)
    .bind(index_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_err)?;
    if missing {
        sqlx::query(
            "INSERT INTO embedding_backfill_jobs (index_id) VALUES ($1)
             ON CONFLICT (index_id) DO UPDATE
             SET status='queued', failure_count=0, lease_token=NULL, lease_until=NULL,
                 completed_at=NULL, next_attempt_at=now(), last_error=NULL, updated_at=now()
             WHERE embedding_backfill_jobs.status IN ('done','failed')",
        )
        .bind(index_id)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }
    Ok(missing)
}

impl Db {
    /// Resolve the index belonging to one exact model vector space. If retained
    /// source text lacks vectors for this index, a durable backfill is queued.
    pub async fn ensure_embedding_index(
        &self,
        org_id: &str,
        embedding_model_id: &str,
        model_version: &str,
        provider: &str,
        dimension: usize,
    ) -> Result<EmbeddingIndex> {
        validate_identity(embedding_model_id, "model id")?;
        validate_identity(model_version, "model version")?;
        validate_identity(provider, "provider")?;
        let dimension = i32::try_from(dimension)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| Error::Model("embedding dimension must fit a positive int".into()))?;

        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let id = memoricai_core::ids::embedding_index_id();
        let inserted = sqlx::query(
            "INSERT INTO embedding_indexes
                (id, org_id, embedding_model_id, model_version, provider, dimension)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (org_id, provider, embedding_model_id, model_version, dimension)
             DO NOTHING",
        )
        .bind(&id)
        .bind(org_id)
        .bind(embedding_model_id)
        .bind(model_version)
        .bind(provider)
        .bind(dimension)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?
        .rows_affected()
            > 0;

        let row = sqlx::query(
            "SELECT id, org_id, embedding_model_id, model_version, provider, dimension
             FROM embedding_indexes
             WHERE org_id=$1 AND provider=$2 AND embedding_model_id=$3
               AND model_version=$4 AND dimension=$5",
        )
        .bind(org_id)
        .bind(provider)
        .bind(embedding_model_id)
        .bind(model_version)
        .bind(dimension)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        let index = EmbeddingIndex {
            id: row.get("id"),
            org_id: row.get("org_id"),
            embedding_model_id: row.get("embedding_model_id"),
            model_version: row.get("model_version"),
            provider: row.get("provider"),
            dimension: row.get::<i32, _>("dimension") as usize,
        };

        if inserted {
            queue_missing_backfill(&mut tx, org_id, &index.id).await?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(index)
    }

    /// Queue or requeue a repair for missing vectors in an existing index.
    /// Normal model migrations call this automatically when the new index is created.
    pub async fn queue_embedding_backfill(&self, org_id: &str, index_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        embedding_index_dimension(&mut tx, index_id, Some(org_id)).await?;
        let queued = queue_missing_backfill(&mut tx, org_id, index_id).await?;
        tx.commit().await.map_err(db_err)?;
        Ok(queued)
    }

    pub async fn organization_ids(&self) -> Result<Vec<String>> {
        sqlx::query_scalar("SELECT id FROM organizations ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)
    }

    pub async fn embedding_indexes(&self, org_id: &str) -> Result<Vec<EmbeddingIndex>> {
        let rows = sqlx::query(
            "SELECT id, org_id, embedding_model_id, model_version, provider, dimension
             FROM embedding_indexes WHERE org_id=$1 ORDER BY created_at, id",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows
            .iter()
            .map(|row| EmbeddingIndex {
                id: row.get("id"),
                org_id: row.get("org_id"),
                embedding_model_id: row.get("embedding_model_id"),
                model_version: row.get("model_version"),
                provider: row.get("provider"),
                dimension: row.get::<i32, _>("dimension") as usize,
            })
            .collect())
    }

    /// Claim one backfill whose exact vector-space identity matches the model
    /// loaded in this process. Multiple replicas coordinate through SKIP LOCKED.
    pub async fn claim_embedding_backfill_for_model(
        &self,
        embedding_model_id: &str,
        model_version: &str,
        provider: &str,
        dimension: usize,
    ) -> Result<Option<(EmbeddingIndex, String)>> {
        let dimension = i32::try_from(dimension)
            .map_err(|_| Error::Model("embedding dimension does not fit an int".into()))?;
        let token = memoricai_core::ids::token(24);
        let row = sqlx::query(
            "WITH candidate AS (
               SELECT job.index_id
               FROM embedding_backfill_jobs job
               JOIN embedding_indexes idx ON idx.id=job.index_id
               WHERE idx.embedding_model_id=$1 AND idx.model_version=$2
                 AND idx.provider=$3 AND idx.dimension=$4
                 AND job.failure_count < 10 AND job.next_attempt_at <= now()
                 AND (job.status='queued'
                      OR (job.status='running' AND job.lease_until < now()))
               ORDER BY job.updated_at, job.index_id
               FOR UPDATE OF job SKIP LOCKED
               LIMIT 1
             ), claimed AS (
               UPDATE embedding_backfill_jobs job
               SET status='running', lease_token=$5,
                   lease_until=now()+interval '5 minutes', updated_at=now(), last_error=NULL
               FROM candidate
               WHERE job.index_id=candidate.index_id
               RETURNING job.index_id
             )
             SELECT idx.id, idx.org_id, idx.embedding_model_id, idx.model_version,
                    idx.provider, idx.dimension
             FROM claimed JOIN embedding_indexes idx ON idx.id=claimed.index_id",
        )
        .bind(embedding_model_id)
        .bind(model_version)
        .bind(provider)
        .bind(dimension)
        .bind(&token)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|row| {
            (
                EmbeddingIndex {
                    id: row.get("id"),
                    org_id: row.get("org_id"),
                    embedding_model_id: row.get("embedding_model_id"),
                    model_version: row.get("model_version"),
                    provider: row.get("provider"),
                    dimension: row.get::<i32, _>("dimension") as usize,
                },
                token,
            )
        }))
    }

    /// Read one bounded batch from a single source table. Keeping memory and chunk
    /// writes in separate transactions avoids lock-order cycles with document
    /// replacement and deletion transactions.
    pub async fn embedding_backfill_batch(
        &self,
        index: &EmbeddingIndex,
        limit: i64,
    ) -> Result<EmbeddingBackfillBatch> {
        let memories: Vec<(String, String)> = sqlx::query(
            "SELECT m.id, m.memory AS content FROM memories m
             WHERE m.org_id=$1 AND NOT EXISTS (
               SELECT 1 FROM memory_embeddings e
               WHERE e.index_id=$2 AND e.memory_id=m.id)
             ORDER BY m.id LIMIT $3",
        )
        .bind(&index.org_id)
        .bind(&index.id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|row| (row.get("id"), row.get("content")))
        .collect();
        if !memories.is_empty() {
            return Ok(EmbeddingBackfillBatch {
                memories,
                chunks: Vec::new(),
            });
        }
        let chunks = sqlx::query(
            "SELECT c.id, c.content FROM chunks c
             WHERE c.org_id=$1 AND NOT EXISTS (
               SELECT 1 FROM chunk_embeddings e
               WHERE e.index_id=$2 AND e.chunk_id=c.id)
             ORDER BY c.id LIMIT $3",
        )
        .bind(&index.org_id)
        .bind(&index.id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|row| (row.get("id"), row.get("content")))
        .collect();
        Ok(EmbeddingBackfillBatch { memories, chunks })
    }

    /// Persist one single-source backfill batch and release its lease atomically.
    /// Empty input marks the job complete; otherwise it is requeued for the next batch.
    pub async fn finish_embedding_backfill_batch(
        &self,
        index_id: &str,
        lease_token: &str,
        memory_ids: &[String],
        memory_embeddings: &[Vec<f32>],
        chunk_ids: &[String],
        chunk_embeddings: &[Vec<f32>],
    ) -> Result<()> {
        if memory_ids.len() != memory_embeddings.len() || chunk_ids.len() != chunk_embeddings.len()
        {
            return Err(Error::Internal(
                "embedding backfill ids and vectors are misaligned".into(),
            ));
        }
        if !memory_ids.is_empty() && !chunk_ids.is_empty() {
            return Err(Error::Internal(
                "embedding backfill batch cannot mix memory and chunk vectors".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let dimension = embedding_index_dimension(&mut tx, index_id, None).await?;
        for embedding in memory_embeddings.iter().chain(chunk_embeddings) {
            validate_stored_embedding(embedding, dimension)?;
        }
        let owned = sqlx::query_scalar::<_, i32>(
            "SELECT 1 FROM embedding_backfill_jobs
             WHERE index_id=$1 AND status='running' AND lease_token=$2 AND lease_until > now()
             FOR UPDATE",
        )
        .bind(index_id)
        .bind(lease_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if owned.is_none() {
            return Err(Error::Conflict(
                "embedding backfill lease is no longer active".into(),
            ));
        }

        if !memory_ids.is_empty() {
            let vectors: Vec<String> = memory_embeddings.iter().map(|v| pgvec(v)).collect();
            sqlx::query(
                "INSERT INTO memory_embeddings (index_id, memory_id, embedding)
                 SELECT $1, input.id, input.embedding::vector
                 FROM unnest($2::text[], $3::text[]) AS input(id, embedding)
                 JOIN memories source ON source.id=input.id
                 ON CONFLICT (index_id, memory_id) DO UPDATE
                 SET embedding=EXCLUDED.embedding, updated_at=now()",
            )
            .bind(index_id)
            .bind(memory_ids)
            .bind(&vectors)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        if !chunk_ids.is_empty() {
            let vectors: Vec<String> = chunk_embeddings.iter().map(|v| pgvec(v)).collect();
            sqlx::query(
                "INSERT INTO chunk_embeddings (index_id, chunk_id, embedding)
                 SELECT $1, input.id, input.embedding::vector
                 FROM unnest($2::text[], $3::text[]) AS input(id, embedding)
                 JOIN chunks source ON source.id=input.id
                 ON CONFLICT (index_id, chunk_id) DO UPDATE
                 SET embedding=EXCLUDED.embedding, updated_at=now()",
            )
            .bind(index_id)
            .bind(chunk_ids)
            .bind(&vectors)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        let complete = memory_ids.is_empty() && chunk_ids.is_empty();
        let updated = sqlx::query(
            "UPDATE embedding_backfill_jobs
             SET status=CASE WHEN $3 THEN 'done' ELSE 'queued' END,
                 processed_memories=processed_memories+$4,
                 processed_chunks=processed_chunks+$5,
                 lease_token=NULL, lease_until=NULL, failure_count=0,
                 next_attempt_at=now(), updated_at=now(),
                 completed_at=CASE WHEN $3 THEN now() ELSE NULL END
             WHERE index_id=$1 AND lease_token=$2",
        )
        .bind(index_id)
        .bind(lease_token)
        .bind(complete)
        .bind(memory_ids.len() as i64)
        .bind(chunk_ids.len() as i64)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if updated.rows_affected() == 0 {
            return Err(Error::Conflict(
                "embedding backfill lease changed during commit".into(),
            ));
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn fail_embedding_backfill(
        &self,
        index_id: &str,
        lease_token: &str,
        error: &str,
    ) -> Result<()> {
        let error: String = error.chars().take(2000).collect();
        sqlx::query(
            "UPDATE embedding_backfill_jobs
             SET failure_count=failure_count+1,
                 status=CASE WHEN failure_count+1 >= 10 THEN 'failed' ELSE 'queued' END,
                 lease_token=NULL, lease_until=NULL,
                 next_attempt_at=now()+interval '30 seconds', last_error=$3, updated_at=now()
             WHERE index_id=$1 AND lease_token=$2",
        )
        .bind(index_id)
        .bind(lease_token)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }
}
