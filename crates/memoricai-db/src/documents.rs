//! Document + chunk repository methods.

use crate::{count_and_rows, db_err, map_document, pgvec, ChunkScore, Db};
use memoricai_core::enums::DocumentStatus;
use memoricai_core::error::{Error, Result};
use memoricai_core::model::Document;
use serde_json::Value;
use sqlx::Row;
use std::collections::HashMap;

/// A chunk ready to persist: (content, position, chunk_type, embedding, metadata).
pub type ChunkDraft = (String, i32, String, Vec<f32>, Value);

impl Db {
    pub async fn insert_document(&self, doc: &Document, raw: Option<&str>) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO documents
               (id, custom_id, content_hash, org_id, user_id, connection_id, title, summary,
                content, raw, url, source, doc_type, status, metadata, container_tags,
                token_count, chunk_count, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)"#,
        )
        .bind(&doc.id)
        .bind(&doc.custom_id)
        .bind(&doc.content_hash)
        .bind(&doc.org_id)
        .bind(&doc.user_id)
        .bind(&doc.connection_id)
        .bind(&doc.title)
        .bind(&doc.summary)
        .bind(&doc.content)
        .bind(raw)
        .bind(&doc.url)
        .bind(&doc.source)
        .bind(&doc.doc_type)
        .bind(doc.status.as_str())
        .bind(&doc.metadata)
        .bind(&doc.container_tags)
        .bind(doc.token_count)
        .bind(doc.chunk_count)
        .bind(doc.created_at)
        .bind(doc.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Fetch a document by internal id or customId within an org.
    pub async fn get_document(&self, org_id: &str, id_or_custom: &str) -> Result<Document> {
        let row = sqlx::query(
            "SELECT * FROM documents WHERE org_id = $1 AND (id = $2 OR custom_id = $2) LIMIT 1",
        )
        .bind(org_id)
        .bind(id_or_custom)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.as_ref()
            .map(map_document)
            .ok_or_else(|| Error::NotFound(format!("document {id_or_custom}")))
    }

    /// Fetch a document by internal id across any tenant (used by ingest workers).
    pub async fn get_document_by_id(&self, id: &str) -> Result<Document> {
        let row = sqlx::query("SELECT * FROM documents WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        row.as_ref()
            .map(map_document)
            .ok_or_else(|| Error::NotFound(format!("document {id}")))
    }

    pub async fn find_document_by_custom_id(
        &self,
        org_id: &str,
        custom_id: &str,
    ) -> Result<Option<Document>> {
        let row =
            sqlx::query("SELECT * FROM documents WHERE org_id = $1 AND custom_id = $2 LIMIT 1")
                .bind(org_id)
                .bind(custom_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(row.as_ref().map(map_document))
    }

    pub async fn documents_by_ids(&self, org_id: &str, ids: &[String]) -> Result<Vec<Document>> {
        let rows = sqlx::query(
            "SELECT * FROM documents WHERE org_id = $1 AND (id = ANY($2) OR custom_id = ANY($2))",
        )
        .bind(org_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_document).collect())
    }

    /// Advance a document's status, fenced by the caller's lease token. Returns
    /// `Error::Conflict` if the lease has been taken over by another worker, so the
    /// stale worker aborts instead of continuing to write.
    pub async fn update_document_status(
        &self,
        id: &str,
        lease_token: &str,
        status: DocumentStatus,
    ) -> Result<()> {
        let r = sqlx::query(
            "UPDATE documents SET status = $2, updated_at = now(),
             lease_until = CASE WHEN $2 IN ('extracting','chunking','embedding','indexing')
                                THEN now() + interval '5 minutes' ELSE NULL END
             WHERE id = $1 AND lease_token = $3",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(lease_token)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        if r.rows_affected() == 0 {
            return Err(Error::Conflict(format!(
                "lease for document {id} was taken over"
            )));
        }
        Ok(())
    }

    /// Atomically claim one queued, retryable, or abandoned ingest job, minting a fresh
    /// lease token. Returns `(document_id, lease_token)`.
    pub async fn claim_next_document(&self) -> Result<Option<(String, String)>> {
        let token = memoricai_core::ids::token(16);
        let row = sqlx::query(
            "UPDATE documents SET status='extracting', processing_attempts=processing_attempts+1,
                    lease_until=now()+interval '5 minutes', lease_token=$1,
                    last_error=NULL, updated_at=now()
             WHERE id = (
               SELECT id FROM documents
               WHERE processing_attempts < 3 AND (
                 status='queued'
                 OR (status IN ('extracting','chunking','embedding','indexing')
                     AND (lease_until IS NULL OR lease_until < now()))
                 OR (status='failed' AND updated_at < now()-interval '30 seconds')
               )
               ORDER BY created_at ASC
               FOR UPDATE SKIP LOCKED
               LIMIT 1
             )
             RETURNING id",
        )
        .bind(&token)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|row| (row.get("id"), token)))
    }

    pub async fn renew_document_lease(&self, id: &str, lease_token: &str) -> Result<()> {
        sqlx::query(
            "UPDATE documents SET lease_until=now()+interval '5 minutes'
             WHERE id=$1 AND lease_token=$2
               AND status IN ('extracting','chunking','embedding','indexing')",
        )
        .bind(id)
        .bind(lease_token)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn mark_document_failed(
        &self,
        id: &str,
        lease_token: &str,
        error: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE documents SET status='failed', lease_until=NULL, lease_token=NULL,
                    last_error=$2, updated_at=now() WHERE id=$1 AND lease_token=$3",
        )
        .bind(id)
        .bind(error)
        .bind(lease_token)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn queue_document_update(
        &self,
        org_id: &str,
        id: &str,
        content: &str,
        content_hash: &str,
        metadata: Option<&Value>,
        allowed_tags: Option<&[String]>,
    ) -> Result<Document> {
        let row = sqlx::query(
            "UPDATE documents SET content=$3, content_hash=$4,
                    metadata=COALESCE($5,metadata), status='queued', processing_attempts=0,
                    lease_until=NULL, last_error=NULL, updated_at=now()
             WHERE org_id=$1 AND (id=$2 OR custom_id=$2)
               AND status NOT IN ('extracting','chunking','embedding','indexing')
               AND ($6::text[] IS NULL OR container_tags <@ $6)
             RETURNING *",
        )
        .bind(org_id)
        .bind(id)
        .bind(content)
        .bind(content_hash)
        .bind(metadata)
        .bind(allowed_tags)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        if let Some(row) = row.as_ref() {
            return Ok(map_document(row));
        }
        match self.get_document(org_id, id).await {
            Ok(document)
                if allowed_tags.is_some_and(|allowed| {
                    document
                        .container_tags
                        .iter()
                        .any(|tag| !allowed.iter().any(|candidate| candidate == tag))
                }) =>
            {
                Err(Error::Forbidden(
                    "document is shared with an unauthorized container".into(),
                ))
            }
            Ok(_) => Err(Error::Conflict(
                "document is currently processing; retry the update".into(),
            )),
            Err(Error::NotFound(_)) => Err(Error::NotFound(format!("document {id}"))),
            Err(error) => Err(error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn queue_document_replacement(
        &self,
        org_id: &str,
        id: &str,
        content: &str,
        content_hash: &str,
        metadata: &Value,
        title: Option<&str>,
        doc_type: &str,
        tags: &[String],
        connection_id: Option<&str>,
        source: &str,
        url: Option<&str>,
        allowed_tags: Option<&[String]>,
    ) -> Result<Document> {
        let row = sqlx::query(
            "UPDATE documents SET content=$3, content_hash=$4, metadata=$5,
                    title=$6, doc_type=$7, container_tags=$8, connection_id=$9,
                    source=$10, url=$11, status='queued', processing_attempts=0,
                    lease_until=NULL, last_error=NULL, updated_at=now()
             WHERE org_id=$1 AND id=$2
               AND status NOT IN ('extracting','chunking','embedding','indexing')
               AND ($12::text[] IS NULL OR container_tags <@ $12)
             RETURNING *",
        )
        .bind(org_id)
        .bind(id)
        .bind(content)
        .bind(content_hash)
        .bind(metadata)
        .bind(title)
        .bind(doc_type)
        .bind(tags)
        .bind(connection_id)
        .bind(source)
        .bind(url)
        .bind(allowed_tags)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        if let Some(row) = row.as_ref() {
            return Ok(map_document(row));
        }
        match self.get_document(org_id, id).await {
            Ok(document)
                if allowed_tags.is_some_and(|allowed| {
                    document
                        .container_tags
                        .iter()
                        .any(|tag| !allowed.iter().any(|candidate| candidate == tag))
                }) =>
            {
                Err(Error::Forbidden(
                    "document is shared with an unauthorized container".into(),
                ))
            }
            Ok(_) => Err(Error::Conflict(
                "document is currently processing; retry the update".into(),
            )),
            Err(Error::NotFound(_)) => Err(Error::NotFound(format!("document {id}"))),
            Err(error) => Err(error),
        }
    }

    /// Mark a document done and record summary + counts.
    pub async fn finish_document(
        &self,
        id: &str,
        lease_token: &str,
        summary: Option<&str>,
        token_count: Option<i64>,
        chunk_count: i64,
    ) -> Result<()> {
        let r = sqlx::query(
            "UPDATE documents SET status = 'done', summary = COALESCE($2, summary),
             token_count = $3, chunk_count = $4, lease_until=NULL, lease_token=NULL,
             last_error=NULL, updated_at = now() WHERE id = $1 AND lease_token = $5",
        )
        .bind(id)
        .bind(summary)
        .bind(token_count)
        .bind(chunk_count)
        .bind(lease_token)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        if r.rows_affected() == 0 {
            return Err(Error::Conflict(format!(
                "lease for document {id} was taken over"
            )));
        }
        Ok(())
    }

    pub async fn patch_document(
        &self,
        org_id: &str,
        id: &str,
        content: Option<&str>,
        metadata: Option<&Value>,
        allowed_tags: Option<&[String]>,
    ) -> Result<Document> {
        let row = sqlx::query(
            "UPDATE documents SET content = COALESCE($3, content),
             metadata = COALESCE($4, metadata), updated_at = now()
             WHERE org_id = $1 AND (id = $2 OR custom_id = $2)
               AND ($5::text[] IS NULL OR container_tags <@ $5)
             RETURNING *",
        )
        .bind(org_id)
        .bind(id)
        .bind(content)
        .bind(metadata)
        .bind(allowed_tags)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        if let Some(row) = row.as_ref() {
            return Ok(map_document(row));
        }
        match self.get_document(org_id, id).await {
            Ok(_) => Err(Error::Forbidden(
                "document is shared with an unauthorized container".into(),
            )),
            Err(Error::NotFound(_)) => Err(Error::NotFound(format!("document {id}"))),
            Err(error) => Err(error),
        }
    }

    /// Delete a document (and its memories/chunks) by id or customId. Returns true if deleted.
    pub async fn delete_document(
        &self,
        org_id: &str,
        id_or_custom: &str,
        allowed_tags: Option<&[String]>,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let row = sqlx::query(
            "SELECT * FROM documents WHERE org_id=$1 AND (id=$2 OR custom_id=$2) FOR UPDATE",
        )
        .bind(org_id)
        .bind(id_or_custom)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let Some(row) = row.as_ref() else {
            tx.rollback().await.map_err(db_err)?;
            return Ok(false);
        };
        let doc = map_document(row);
        if allowed_tags.is_some_and(|allowed| {
            doc.container_tags
                .iter()
                .any(|tag| !allowed.iter().any(|candidate| candidate == tag))
        }) {
            tx.rollback().await.map_err(db_err)?;
            return Err(Error::Forbidden(
                "document is shared with an unauthorized container".into(),
            ));
        }
        sqlx::query("DELETE FROM chunks WHERE document_id = $1")
            .bind(&doc.id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        crate::memories::prepare_memories_for_document_deletion(
            &mut tx,
            std::slice::from_ref(&doc.id),
        )
        .await?;
        sqlx::query("DELETE FROM documents WHERE id = $1")
            .bind(&doc.id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(true)
    }

    pub async fn bulk_delete_by_ids(
        &self,
        org_id: &str,
        ids: &[String],
        allowed_tags: Option<&[String]>,
    ) -> Result<u64> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let document_ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM documents
             WHERE org_id=$1 AND (id=ANY($2) OR custom_id=ANY($2))
               AND ($3::text[] IS NULL OR container_tags <@ $3)
             FOR UPDATE",
        )
        .bind(org_id)
        .bind(ids)
        .bind(allowed_tags)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        crate::memories::prepare_memories_for_document_deletion(&mut tx, &document_ids).await?;
        let r = sqlx::query("DELETE FROM documents WHERE id=ANY($1)")
            .bind(&document_ids)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(r.rows_affected())
    }

    pub async fn bulk_delete_by_tags(
        &self,
        org_id: &str,
        tags: &[String],
        allowed_tags: Option<&[String]>,
    ) -> Result<u64> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let document_ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM documents
             WHERE org_id=$1 AND container_tags && $2
               AND ($3::text[] IS NULL OR container_tags <@ $3)
             FOR UPDATE",
        )
        .bind(org_id)
        .bind(tags)
        .bind(allowed_tags)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        crate::memories::prepare_memories_for_document_deletion(&mut tx, &document_ids).await?;
        let r = sqlx::query("DELETE FROM documents WHERE id=ANY($1)")
            .bind(&document_ids)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(r.rows_affected())
    }

    pub async fn list_processing(
        &self,
        org_id: &str,
        tags: Option<&[String]>,
    ) -> Result<Vec<Document>> {
        let rows = sqlx::query(
            "SELECT * FROM documents WHERE org_id = $1
             AND ($2::text[] IS NULL OR container_tags && $2)
             AND status IN ('queued','extracting','chunking','embedding','indexing')
             ORDER BY created_at DESC LIMIT 200",
        )
        .bind(org_id)
        .bind(tags)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_document).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_documents(
        &self,
        org_id: &str,
        tags: Option<&[String]>,
        status: Option<&str>,
        page: u32,
        limit: u32,
        sort: &str,
        order: &str,
    ) -> Result<(Vec<Document>, u64)> {
        let sort_col = match sort {
            "updatedAt" | "updated_at" => "updated_at",
            _ => "created_at",
        };
        let order_kw = if order.eq_ignore_ascii_case("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let offset = (page.saturating_sub(1) as i64) * limit as i64;

        let where_sql = "WHERE org_id = $1
             AND ($2::text[] IS NULL OR container_tags && $2)
             AND ($3::text IS NULL OR status = $3)";

        let count_sql = format!("SELECT count(*) AS c FROM documents {where_sql}");
        let count_q = sqlx::query(&count_sql).bind(org_id).bind(tags).bind(status);

        let list_sql = format!(
            "SELECT * FROM documents {where_sql} ORDER BY {sort_col} {order_kw} LIMIT $4 OFFSET $5"
        );
        let rows_q = sqlx::query(&list_sql)
            .bind(org_id)
            .bind(tags)
            .bind(status)
            .bind(limit as i64)
            .bind(offset);

        let (count, rows) = count_and_rows(&self.pool, count_q, rows_q).await?;
        Ok((rows.iter().map(map_document).collect(), count as u64))
    }

    pub async fn count_documents_for_connection(
        &self,
        org_id: &str,
        connection_id: &str,
    ) -> Result<i64> {
        let count: i64 =
            sqlx::query("SELECT count(*) AS c FROM documents WHERE org_id=$1 AND connection_id=$2")
                .bind(org_id)
                .bind(connection_id)
                .fetch_one(&self.pool)
                .await
                .map_err(db_err)?
                .get("c");
        Ok(count)
    }

    /// Document counts per container tag for an org. Tags with no documents are absent.
    pub async fn count_documents_by_tag(&self, org_id: &str) -> Result<HashMap<String, i64>> {
        let rows = sqlx::query(
            "SELECT t.tag AS tag, count(DISTINCT id) AS c
             FROM documents, unnest(container_tags) AS t(tag)
             WHERE org_id = $1
             GROUP BY t.tag",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(|r| (r.get("tag"), r.get("c"))).collect())
    }

    // ---------- chunks ----------

    /// Atomically replace every chunk copy for all of a document's container tags.
    pub async fn replace_chunks_for_document(
        &self,
        document_id: &str,
        org_id: &str,
        container_tags: &[String],
        drafts: &[ChunkDraft],
    ) -> Result<usize> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query("DELETE FROM chunks WHERE document_id=$1")
            .bind(document_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if drafts.is_empty() || container_tags.is_empty() {
            tx.commit().await.map_err(db_err)?;
            return Ok(0);
        }
        let count = drafts.len().saturating_mul(container_tags.len());
        let mut ids = Vec::with_capacity(count);
        let mut tags = Vec::with_capacity(count);
        let mut contents = Vec::with_capacity(count);
        let mut chunk_types = Vec::with_capacity(count);
        let mut positions = Vec::with_capacity(count);
        let mut embeddings = Vec::with_capacity(count);
        let mut metadatas = Vec::with_capacity(count);
        for tag in container_tags {
            for (content, position, chunk_type, embedding, metadata) in drafts {
                ids.push(memoricai_core::ids::chunk_id());
                tags.push(tag.as_str());
                contents.push(content.as_str());
                chunk_types.push(chunk_type.as_str());
                positions.push(*position);
                embeddings.push(pgvec(embedding));
                metadatas.push(metadata);
            }
        }
        sqlx::query(
            r#"INSERT INTO chunks
               (id, document_id, org_id, space_container_tag, content, chunk_type, position, embedding, metadata)
               SELECT t.id, $2, $3, t.tag, t.content, t.chunk_type, t.position, t.embedding::vector, t.metadata
               FROM unnest($1::text[], $4::text[], $5::text[], $6::text[], $7::int4[], $8::text[], $9::jsonb[])
                    AS t(id, tag, content, chunk_type, position, embedding, metadata)"#,
        )
        .bind(&ids)
        .bind(document_id)
        .bind(org_id)
        .bind(&tags)
        .bind(&contents)
        .bind(&chunk_types)
        .bind(&positions)
        .bind(&embeddings)
        .bind(&metadatas)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(count)
    }

    pub async fn search_chunks(
        &self,
        org_id: &str,
        tags: Option<&[String]>,
        qvec: &[f32],
        k: i64,
        threshold: f32,
        doc_id: Option<&str>,
    ) -> Result<Vec<ChunkScore>> {
        let rows = sqlx::query(
            r#"SELECT c.document_id, c.content, d.metadata AS doc_metadata,
                      1 - (c.embedding <=> $1::vector) AS similarity
               FROM chunks c
               JOIN documents d ON d.id = c.document_id AND d.org_id = c.org_id
               WHERE c.org_id = $2
                 -- Only surface chunks of fully-indexed documents (a failed / in-progress
                 -- reindex must not appear in results).
                 AND d.status = 'done'
                 AND ($3::text[] IS NULL OR c.space_container_tag = ANY($3))
                 AND ($4::text IS NULL OR c.document_id = $4)
                 AND c.embedding IS NOT NULL
                 AND 1 - (c.embedding <=> $1::vector) >= $5
               ORDER BY c.embedding <=> $1::vector
               LIMIT $6"#,
        )
        .bind(pgvec(qvec))
        .bind(org_id)
        .bind(tags)
        .bind(doc_id)
        .bind(threshold as f64)
        .bind(k)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(rows
            .iter()
            .map(|row| ChunkScore {
                document_id: row.get("document_id"),
                content: row.get("content"),
                doc_metadata: row.get("doc_metadata"),
                similarity: row.get::<f64, _>("similarity") as f32,
            })
            .collect())
    }
}
