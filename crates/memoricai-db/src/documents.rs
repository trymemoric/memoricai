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

// `raw` can be as large as the source document and is intentionally excluded:
// no Document API field consumes it. Search-only projections also omit content.
const DOCUMENT_COLUMNS: &str = "id, custom_id, content_hash, org_id, user_id, connection_id, \
    title, summary, content, url, source, doc_type, status, metadata, container_tags, \
    token_count, chunk_count, created_at, updated_at";
const DOCUMENT_SUMMARY_COLUMNS: &str = "id, custom_id, content_hash, org_id, user_id, connection_id, \
    title, summary, NULL::text AS content, url, source, doc_type, status, metadata, container_tags, \
    token_count, chunk_count, created_at, updated_at";

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
        let sql = format!(
            "SELECT {DOCUMENT_COLUMNS} FROM documents \
             WHERE org_id = $1 AND (id = $2 OR custom_id = $2) LIMIT 1"
        );
        let row = sqlx::query(&sql)
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
        let sql = format!("SELECT {DOCUMENT_COLUMNS} FROM documents WHERE id = $1");
        let row = sqlx::query(&sql)
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
        let sql = format!(
            "SELECT {DOCUMENT_SUMMARY_COLUMNS} FROM documents \
             WHERE org_id = $1 AND custom_id = $2 LIMIT 1"
        );
        let row = sqlx::query(&sql)
            .bind(org_id)
            .bind(custom_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_document))
    }

    pub async fn document_exists_by_custom_id(
        &self,
        org_id: &str,
        custom_id: &str,
    ) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM documents WHERE org_id=$1 AND custom_id=$2)",
        )
        .bind(org_id)
        .bind(custom_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)
    }

    pub async fn documents_by_ids(&self, org_id: &str, ids: &[String]) -> Result<Vec<Document>> {
        self.documents_by_ids_projected(org_id, ids, true).await
    }

    /// Batch-fetch documents without their large content field when callers only
    /// need metadata, dates, titles, or source attribution.
    pub async fn document_summaries_by_ids(
        &self,
        org_id: &str,
        ids: &[String],
    ) -> Result<Vec<Document>> {
        self.documents_by_ids_projected(org_id, ids, false).await
    }

    async fn documents_by_ids_projected(
        &self,
        org_id: &str,
        ids: &[String],
        include_content: bool,
    ) -> Result<Vec<Document>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let columns = if include_content {
            DOCUMENT_COLUMNS
        } else {
            DOCUMENT_SUMMARY_COLUMNS
        };
        let sql = format!(
            "SELECT {columns} FROM documents \
             WHERE org_id = $1 AND (id = ANY($2) OR custom_id = ANY($2))"
        );
        let rows = sqlx::query(&sql)
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
             WHERE id = $1 AND lease_token = $3 AND lease_until > now()",
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
        let result = sqlx::query(
            "UPDATE documents SET lease_until=now()+interval '5 minutes'
             WHERE id=$1 AND lease_token=$2
               AND lease_until > now()
               AND status IN ('extracting','chunking','embedding','indexing')",
        )
        .bind(id)
        .bind(lease_token)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        if result.rows_affected() == 0 {
            return Err(Error::Conflict(format!(
                "lease for document {id} is no longer active"
            )));
        }
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
                    last_error=$2, updated_at=now()
             WHERE id=$1 AND lease_token=$3 AND lease_until > now()",
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
        let row = sqlx::query(&format!(
            "UPDATE documents SET content=$3, content_hash=$4,
                    metadata=COALESCE($5,metadata), status='queued', processing_attempts=0,
                    lease_until=NULL, lease_token=NULL, last_error=NULL, updated_at=now()
             WHERE org_id=$1 AND (id=$2 OR custom_id=$2)
               AND status NOT IN ('extracting','chunking','embedding','indexing')
               AND ($6::text[] IS NULL OR container_tags <@ $6)
             RETURNING {DOCUMENT_COLUMNS}"
        ))
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
        let row = sqlx::query(&format!(
            "UPDATE documents SET content=$3, content_hash=$4, metadata=$5,
                    title=$6, doc_type=$7, container_tags=$8, connection_id=$9,
                    source=$10, url=$11, status='queued', processing_attempts=0,
                    lease_until=NULL, lease_token=NULL, last_error=NULL, updated_at=now()
             WHERE org_id=$1 AND id=$2
               AND status NOT IN ('extracting','chunking','embedding','indexing')
               AND ($12::text[] IS NULL OR container_tags <@ $12)
             RETURNING {DOCUMENT_COLUMNS}"
        ))
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

    pub async fn patch_document(
        &self,
        org_id: &str,
        id: &str,
        content: Option<&str>,
        metadata: Option<&Value>,
        allowed_tags: Option<&[String]>,
    ) -> Result<Document> {
        let row = sqlx::query(&format!(
            "UPDATE documents SET content = COALESCE($3, content),
             metadata = COALESCE($4, metadata), updated_at = now()
             WHERE org_id = $1 AND (id = $2 OR custom_id = $2)
               AND ($5::text[] IS NULL OR container_tags <@ $5)
             RETURNING {DOCUMENT_COLUMNS}"
        ))
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
        let sql = format!(
            "SELECT {DOCUMENT_SUMMARY_COLUMNS} FROM documents \
             WHERE org_id=$1 AND (id=$2 OR custom_id=$2) FOR UPDATE"
        );
        let row = sqlx::query(&sql)
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
        let sql = format!(
            "SELECT {DOCUMENT_COLUMNS} FROM documents WHERE org_id = $1
             AND ($2::text[] IS NULL OR container_tags && $2)
             AND status IN ('queued','extracting','chunking','embedding','indexing')
             ORDER BY created_at DESC LIMIT 200"
        );
        let rows = sqlx::query(&sql)
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
            "SELECT {DOCUMENT_COLUMNS} FROM documents {where_sql} \
             ORDER BY {sort_col} {order_kw} LIMIT $4 OFFSET $5"
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

    /// Delete documents for a connection whose external id was NOT enumerated at the source
    /// this run (`custom_id = "{connection_id}:{external_id}"`), so content deleted upstream
    /// stops being searchable. The caller only invokes this after a complete enumeration;
    /// the empty set is therefore meaningful and removes every document for an empty source.
    /// Selection, graph repair, and deletion are one transaction.
    pub async fn reconcile_connection_documents(
        &self,
        org_id: &str,
        connection_id: &str,
        seen_external_ids: &[String],
    ) -> Result<u64> {
        let seen_custom_ids: Vec<String> = seen_external_ids
            .iter()
            .map(|e| format!("{connection_id}:{e}"))
            .collect();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let stale_ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM documents
             WHERE org_id=$1 AND connection_id=$2
               AND (custom_id IS NULL OR NOT (custom_id = ANY($3)))
             FOR UPDATE",
        )
        .bind(org_id)
        .bind(connection_id)
        .bind(&seen_custom_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        if stale_ids.is_empty() {
            tx.commit().await.map_err(db_err)?;
            return Ok(0);
        }
        crate::memories::prepare_memories_for_document_deletion(&mut tx, &stale_ids).await?;
        let deleted = sqlx::query("DELETE FROM documents WHERE id=ANY($1)")
            .bind(&stale_ids)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
            .rows_affected();
        tx.commit().await.map_err(db_err)?;
        Ok(deleted)
    }

    /// Delete one provider item reported by an incremental change feed.
    pub async fn delete_connection_document(
        &self,
        org_id: &str,
        connection_id: &str,
        external_id: &str,
    ) -> Result<bool> {
        let custom_id = format!("{connection_id}:{external_id}");
        self.delete_document(org_id, &custom_id, None).await
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

    /// Atomically publish a fully prepared document index. The live lease is locked and
    /// revalidated before any mutation, and the final `done` transition is committed with
    /// chunk replacement, memory replacement, graph edges, and bucket assignments.
    #[allow(clippy::too_many_arguments)]
    pub async fn replace_document_index(
        &self,
        document_id: &str,
        lease_token: &str,
        org_id: &str,
        embedding_index_id: &str,
        container_tags: &[String],
        chunk_drafts: &[ChunkDraft],
        memory_drafts: &[crate::memories::ExtractedMemoryDraft],
        summary: Option<&str>,
        token_count: Option<i64>,
        chunk_count: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let owned = sqlx::query_scalar::<_, i32>(
            "SELECT 1 FROM documents
             WHERE id=$1 AND org_id=$2 AND lease_token=$3 AND lease_until > now()
               AND status='indexing'
             FOR UPDATE",
        )
        .bind(document_id)
        .bind(org_id)
        .bind(lease_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if owned.is_none() {
            return Err(Error::Conflict(format!(
                "lease for document {document_id} is no longer active"
            )));
        }
        let embedding_dimension =
            crate::embeddings::embedding_index_dimension(&mut tx, embedding_index_id, Some(org_id))
                .await?;
        for (_, _, _, embedding, _) in chunk_drafts {
            crate::embeddings::validate_stored_embedding(embedding, embedding_dimension)?;
        }
        for draft in memory_drafts {
            crate::embeddings::validate_stored_embedding(&draft.embedding, embedding_dimension)?;
        }

        sqlx::query("DELETE FROM chunks WHERE document_id=$1")
            .bind(document_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        if !chunk_drafts.is_empty() && !container_tags.is_empty() {
            let count = chunk_drafts.len();
            let mut ids = Vec::with_capacity(count);
            let mut contents = Vec::with_capacity(count);
            let mut chunk_types = Vec::with_capacity(count);
            let mut positions = Vec::with_capacity(count);
            let mut embeddings = Vec::with_capacity(count);
            let mut metadatas = Vec::with_capacity(count);
            for (content, position, chunk_type, embedding, metadata) in chunk_drafts {
                ids.push(memoricai_core::ids::chunk_id());
                contents.push(content.as_str());
                chunk_types.push(chunk_type.as_str());
                positions.push(*position);
                embeddings.push(pgvec(embedding));
                metadatas.push(metadata);
            }
            sqlx::query(
                r#"INSERT INTO chunks
                   (id, document_id, org_id, content, chunk_type, position, metadata)
                   SELECT t.id, $2, $3, t.content, t.chunk_type, t.position, t.metadata
                   FROM unnest($1::text[], $4::text[], $5::text[], $6::int4[], $7::jsonb[])
                        AS t(id, content, chunk_type, position, metadata)"#,
            )
            .bind(&ids)
            .bind(document_id)
            .bind(org_id)
            .bind(&contents)
            .bind(&chunk_types)
            .bind(&positions)
            .bind(&metadatas)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            let membership_count = ids.len().saturating_mul(container_tags.len());
            let mut membership_ids = Vec::with_capacity(membership_count);
            let mut membership_tags = Vec::with_capacity(membership_count);
            for tag in container_tags {
                for id in &ids {
                    membership_ids.push(id.as_str());
                    membership_tags.push(tag.as_str());
                }
            }
            sqlx::query(
                "INSERT INTO chunk_containers (chunk_id, container_tag)
                 SELECT * FROM unnest($1::text[], $2::text[])",
            )
            .bind(&membership_ids)
            .bind(&membership_tags)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "INSERT INTO chunk_embeddings (index_id, chunk_id, embedding)
                 SELECT $1, input.id, input.embedding::vector
                 FROM unnest($2::text[], $3::text[]) AS input(id, embedding)",
            )
            .bind(embedding_index_id)
            .bind(&ids)
            .bind(&embeddings)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        crate::memories::prepare_memories_for_document_deletion(
            &mut tx,
            &[document_id.to_string()],
        )
        .await?;
        crate::memories::insert_extracted_memories(
            &mut tx,
            document_id,
            org_id,
            embedding_index_id,
            embedding_dimension,
            memory_drafts,
        )
        .await?;

        let finished = sqlx::query(
            "UPDATE documents SET status='done', summary=COALESCE($2,summary),
                    token_count=$3, chunk_count=$4, lease_until=NULL, lease_token=NULL,
                    last_error=NULL, updated_at=now()
             WHERE id=$1 AND lease_token=$5 AND lease_until > now()",
        )
        .bind(document_id)
        .bind(summary)
        .bind(token_count)
        .bind(chunk_count)
        .bind(lease_token)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        if finished.rows_affected() == 0 {
            return Err(Error::Conflict(format!(
                "lease for document {document_id} expired during index commit"
            )));
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn search_chunks(
        &self,
        org_id: &str,
        embedding_index_id: &str,
        embedding_dimension: usize,
        tags: Option<&[String]>,
        qvec: &[f32],
        k: i64,
        threshold: f32,
        doc_id: Option<&str>,
    ) -> Result<Vec<ChunkScore>> {
        let vector_column = if tags.is_some() {
            "scoped.embedding"
        } else {
            "ce.embedding"
        };
        let (stored_vector, query_vector) =
            crate::embeddings::vector_search_operands(vector_column, embedding_dimension)?;
        let index_id = crate::embeddings::sql_text_literal(embedding_index_id);
        let sql = if tags.is_some() {
            // pgvector applies ordinary WHERE filters after an approximate HNSW scan. A
            // membership-table filter can therefore exhaust the ANN candidate list before
            // finding k rows from a small container. Materialize the authorized subset and
            // rank it exactly so changing the storage representation cannot change recall.
            format!(
                r#"WITH scoped AS MATERIALIZED (
                       SELECT c.document_id, c.content, d.metadata AS doc_metadata, ce.embedding
                       FROM chunks c
                       JOIN chunk_embeddings ce
                         ON ce.chunk_id=c.id AND ce.index_id={index_id}
                       JOIN documents d ON d.id=c.document_id AND d.org_id=c.org_id
                       WHERE c.org_id=$2 AND d.status='done'
                         AND EXISTS (
                             SELECT 1 FROM chunk_containers membership
                             WHERE membership.chunk_id=c.id
                               AND membership.container_tag=ANY($3)
                         )
                         AND ($4::text IS NULL OR c.document_id=$4)
                   )
                   SELECT scoped.document_id, scoped.content, scoped.doc_metadata,
                          1 - ({stored_vector} <=> {query_vector}) AS similarity
                   FROM scoped
                   WHERE 1 - ({stored_vector} <=> {query_vector}) >= $5
                   ORDER BY {stored_vector} <=> {query_vector}
                   LIMIT $6"#,
            )
        } else {
            format!(
                r#"SELECT c.document_id, c.content, d.metadata AS doc_metadata,
                      1 - ({stored_vector} <=> {query_vector}) AS similarity
               FROM chunks c
               JOIN chunk_embeddings ce ON ce.chunk_id=c.id AND ce.index_id={index_id}
               JOIN documents d ON d.id = c.document_id AND d.org_id = c.org_id
               WHERE c.org_id = $2
                 -- Only surface chunks of fully-indexed documents (a failed / in-progress
                 -- reindex must not appear in results).
                 AND d.status = 'done'
                 AND $3::text[] IS NULL
                 AND ($4::text IS NULL OR c.document_id = $4)
                 AND 1 - ({stored_vector} <=> {query_vector}) >= $5
               ORDER BY {stored_vector} <=> {query_vector}
               LIMIT $6"#,
            )
        };
        let rows = sqlx::query(&sql)
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
