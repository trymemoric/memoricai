//! The staged ingest pipeline: extract → chunk → embed → index → extract
//! memories → build graph → finalize. Runs on the background worker pool.

use crate::{chunk, extract, Engine};
use memoricai_core::enums::DocumentStatus;
use memoricai_core::error::Result;

impl Engine {
    /// Advance a queued document through all pipeline stages. All status/lease writes are
    /// fenced by `lease_token`, so a stale worker whose lease was reclaimed aborts.
    pub async fn process_document(&self, doc_id: &str, lease_token: &str) -> Result<()> {
        let doc = self.db.get_document_by_id(doc_id).await?;
        let org_id = doc.org_id.clone();
        let settings = self.db.get_settings(&org_id).await?;
        let tags = if doc.container_tags.is_empty() {
            vec![memoricai_core::DEFAULT_CONTAINER_TAG.to_string()]
        } else {
            doc.container_tags.clone()
        };
        let content = doc.content.as_deref().unwrap_or_default();

        // --- extract ---
        self.db
            .update_document_status(doc_id, lease_token, DocumentStatus::Extracting)
            .await?;
        let extracted = extract::extract(&doc.doc_type, content, doc.url.as_deref()).await?;
        let text = extracted.text;

        // --- chunk ---
        self.db
            .update_document_status(doc_id, lease_token, DocumentStatus::Chunking)
            .await?;
        let chunk_chars = usize::try_from(settings.chunk_size)
            .ok()
            .filter(|size| *size > 0)
            .unwrap_or(self.config.chunk_chars);
        let pieces = chunk::chunk_text(&text, &doc.doc_type, chunk_chars);

        // --- embed chunks and prepare memories before replacing the live index ---
        self.db
            .update_document_status(doc_id, lease_token, DocumentStatus::Embedding)
            .await?;
        let chunk_count = pieces.len();
        let drafts = if !pieces.is_empty() {
            let contents: Vec<String> = pieces.iter().map(|(c, _, _)| c.clone()).collect();
            let embeddings = self.models.embedder.embed_batch(&contents).await?;
            crate::validate_embedding_batch(&contents, &embeddings, self.models.dim())?;
            pieces
                .into_iter()
                .zip(embeddings)
                .map(|((content, pos, ctype), emb)| {
                    (content, pos, ctype, emb, serde_json::json!({}))
                })
                .collect::<Vec<memoricai_db::documents::ChunkDraft>>()
        } else {
            Vec::new()
        };

        let mut prepared_memories = Vec::with_capacity(tags.len());
        for tag in &tags {
            let entity_context = self
                .db
                .get_space(&org_id, tag)
                .await?
                .and_then(|space| space.entity_context);
            let facts = self
                .extract_memories(&text, entity_context.as_deref(), Some(&settings))
                .await?;
            let fact_texts: Vec<String> = facts.iter().map(|fact| fact.content.clone()).collect();
            let embeddings = self.models.embedder.embed_batch(&fact_texts).await?;
            crate::validate_embedding_batch(&fact_texts, &embeddings, self.models.dim())?;
            prepared_memories.push((tag.clone(), facts, embeddings));
        }

        // --- atomically replace chunks, then rebuild the document's memory graph ---
        self.db
            .update_document_status(doc_id, lease_token, DocumentStatus::Indexing)
            .await?;
        self.db
            .replace_chunks_for_document(doc_id, &org_id, &tags, &drafts)
            .await?;
        // Idempotent reprocess: clear any memories previously derived from this doc.
        self.db.delete_memories_for_document(doc_id).await?;
        for (tag, facts, fact_embeddings) in prepared_memories {
            for (fact, emb) in facts.iter().zip(&fact_embeddings) {
                // Sequential so relation inference sees prior facts from this doc.
                let mem_id = self
                    .store_extracted(&org_id, doc.user_id.as_deref(), doc_id, &tag, fact, emb)
                    .await?;
                // Bucket classification (best-effort; skipped if no buckets defined).
                if let Ok(Some(bucket)) = self.classify_bucket(&org_id, &tag, &fact.content).await {
                    let _ = self.db.set_memory_bucket(&mem_id, &bucket).await;
                }
            }
        }

        // --- finalize ---
        let summary = make_summary(&text);
        let token_count = (text.len() / 4) as i64;
        self.db
            .finish_document(
                doc_id,
                lease_token,
                summary.as_deref(),
                Some(token_count),
                chunk_count as i64,
            )
            .await?;
        Ok(())
    }
}

/// Cheap extractive summary (Phase 1): the leading text, truncated on a word boundary.
fn make_summary(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    const MAX: usize = 300;
    if trimmed.len() <= MAX {
        return Some(trimmed.to_string());
    }
    let mut end = MAX;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let cut = trimmed[..end].rfind(' ').unwrap_or(end);
    Some(format!("{}…", &trimmed[..cut]))
}
