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
        let embedding_index = self.embedding_index(&org_id).await?;
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

        // Bound the text handed to the extraction LLM so a large (but within-limit)
        // document cannot exceed the model context window and fail the entire ingest.
        // Chunk embeddings, which power search, still cover the full document; only the
        // extracted atomic memories are drawn from the leading portion.
        let extraction_input: &str = {
            const MAX_EXTRACTION_BYTES: usize = 100 * 1024;
            if text.len() <= MAX_EXTRACTION_BYTES {
                &text
            } else {
                let mut end = MAX_EXTRACTION_BYTES;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                &text[..end]
            }
        };

        let mut prepared_memories = Vec::new();
        for tag in &tags {
            let entity_context = self
                .db
                .get_space(&org_id, tag)
                .await?
                .and_then(|space| space.entity_context);
            let facts = self
                .extract_memories(extraction_input, entity_context.as_deref(), Some(&settings))
                .await?;
            let fact_texts: Vec<String> = facts.iter().map(|fact| fact.content.clone()).collect();
            let embeddings = self.models.embedder.embed_batch(&fact_texts).await?;
            crate::validate_embedding_batch(&fact_texts, &embeddings, self.models.dim())?;
            for (fact, embedding) in facts.into_iter().zip(embeddings) {
                let bucket_key = self
                    .classify_bucket(&org_id, tag, &fact.content)
                    .await
                    .ok()
                    .flatten();
                prepared_memories.push(memoricai_db::memories::ExtractedMemoryDraft {
                    user_id: doc.user_id.clone(),
                    container_tag: tag.clone(),
                    content: fact.content,
                    embedding,
                    is_static: fact.is_static,
                    forget_after: fact
                        .forget_after
                        .as_deref()
                        .and_then(crate::memory::parse_iso_date),
                    event_date: fact
                        .event_date
                        .as_deref()
                        .and_then(crate::memory::parse_iso_date),
                    bucket_key,
                });
            }
        }

        // --- atomically publish chunks, memories, graph, buckets, and final status ---
        self.db
            .update_document_status(doc_id, lease_token, DocumentStatus::Indexing)
            .await?;
        let summary = make_summary(&text);
        let token_count = (text.len() / 4) as i64;
        self.db
            .replace_document_index(
                doc_id,
                lease_token,
                &org_id,
                &embedding_index.id,
                &tags,
                &drafts,
                &prepared_memories,
                summary.as_deref(),
                Some(token_count),
                chunk_count as i64,
            )
            .await?;
        Ok(())
    }
}

/// Cheap extractive summary: the leading text, truncated on a word boundary.
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
