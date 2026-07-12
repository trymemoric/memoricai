//! memoricai-engine: the ingestion pipeline, memory extraction, temporal graph,
//! search, and profiles. `Engine` is the facade the API + MCP build on.

pub mod chunk;
pub mod extract;
pub mod media;
pub mod memory;
pub mod pipeline;
pub mod search;

use std::sync::Arc;

use memoricai_core::dto::{
    CreateMemoriesRequest, CreateMemoriesResponse, CreatedMemory, IngestRequest,
};
use memoricai_core::enums::DocumentStatus;
use memoricai_core::error::{Error, Result};
use memoricai_core::model::{Document, Memory, Timestamp};
use memoricai_db::Db;
use memoricai_models::ModelStack;
use tokio::sync::{mpsc, Semaphore};

pub const MAX_DOCUMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_MEMORY_BYTES: usize = 10 * 1024;

/// Aborts a spawned task when dropped — including during panic unwind — so a
/// lease-heartbeat can never outlive the job it was renewing.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn validate_metadata(metadata: &serde_json::Value) -> Result<()> {
    if serde_json::to_vec(metadata)
        .map_err(|error| Error::BadRequest(format!("invalid metadata: {error}")))?
        .len()
        > MAX_METADATA_BYTES
    {
        return Err(Error::BadRequest("metadata exceeds 256 KiB".into()));
    }
    Ok(())
}

pub(crate) fn validate_embedding_batch(
    texts: &[String],
    embeddings: &[Vec<f32>],
    expected_dim: usize,
) -> Result<()> {
    if embeddings.len() != texts.len() {
        return Err(Error::Model(format!(
            "embedding provider returned {} vectors for {} inputs",
            embeddings.len(),
            texts.len()
        )));
    }
    for embedding in embeddings {
        validate_embedding(embedding, expected_dim)?;
    }
    Ok(())
}

pub(crate) fn validate_embedding(embedding: &[f32], expected_dim: usize) -> Result<()> {
    if embedding.len() != expected_dim {
        return Err(Error::Model(format!(
            "embedding dimension {} does not match configured dimension {expected_dim}",
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

#[derive(Clone)]
pub struct EngineConfig {
    pub ingest_concurrency: usize,
    /// Approximate target chunk size in characters.
    pub chunk_chars: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            ingest_concurrency: 2,
            chunk_chars: 1200,
        }
    }
}

/// Bounded FIFO cache for query embeddings. Remote embedding round-trips
/// dominate search latency, so repeated queries (agent loops, retries,
/// profile searches) should not pay for the same vector twice.
struct QueryEmbeddingCache {
    map: std::collections::HashMap<String, Vec<f32>>,
    order: std::collections::VecDeque<String>,
    cap: usize,
}

impl QueryEmbeddingCache {
    fn new(cap: usize) -> Self {
        Self {
            map: Default::default(),
            order: Default::default(),
            cap,
        }
    }

    fn get(&self, q: &str) -> Option<Vec<f32>> {
        self.map.get(q).cloned()
    }

    fn put(&mut self, q: String, embedding: Vec<f32>) {
        if self.map.contains_key(&q) {
            return;
        }
        if self.map.len() >= self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
        self.order.push_back(q.clone());
        self.map.insert(q, embedding);
    }
}

#[derive(Clone)]
pub struct Engine {
    pub db: Db,
    pub models: Arc<ModelStack>,
    pub config: EngineConfig,
    tx: mpsc::Sender<()>,
    sem: Arc<Semaphore>,
    query_cache: Arc<std::sync::Mutex<QueryEmbeddingCache>>,
}

impl Engine {
    /// Construct the engine and spawn its background ingest dispatcher.
    /// Must be called inside a tokio runtime.
    pub fn new(db: Db, models: Arc<ModelStack>, config: EngineConfig) -> Self {
        let (tx, rx) = mpsc::channel::<()>(1);
        let sem = Arc::new(Semaphore::new(config.ingest_concurrency.max(1)));
        let engine = Self {
            db,
            models,
            config,
            tx,
            sem,
            query_cache: Arc::new(std::sync::Mutex::new(QueryEmbeddingCache::new(512))),
        };
        let dispatcher = engine.clone();
        tokio::spawn(async move { dispatcher.dispatch_loop(rx).await });
        engine
    }

    /// Claim durable jobs from Postgres and process them with bounded concurrency.
    async fn dispatch_loop(self, mut rx: mpsc::Receiver<()>) {
        let mut poll = tokio::time::interval(std::time::Duration::from_millis(500));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            while let Ok(permit) = self.sem.clone().try_acquire_owned() {
                let (doc_id, lease_token) = match self.db.claim_next_document().await {
                    Ok(Some(pair)) => pair,
                    Ok(None) => {
                        drop(permit);
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to claim ingest job");
                        drop(permit);
                        break;
                    }
                };
                let engine = self.clone();
                tokio::spawn(async move {
                    let heartbeat_engine = engine.clone();
                    let heartbeat_doc_id = doc_id.clone();
                    let heartbeat_token = lease_token.clone();
                    let heartbeat = tokio::spawn(async move {
                        let mut interval =
                            tokio::time::interval(std::time::Duration::from_secs(60));
                        interval.tick().await;
                        loop {
                            interval.tick().await;
                            // A single transient renewal error (e.g. momentary pool
                            // exhaustion) must not silence the heartbeat for the rest of a
                            // long stage; log and keep renewing. Fenced by the lease token.
                            if let Err(error) = heartbeat_engine
                                .db
                                .renew_document_lease(&heartbeat_doc_id, &heartbeat_token)
                                .await
                            {
                                tracing::warn!(doc_id = %heartbeat_doc_id, %error, "lease renewal failed");
                            }
                        }
                    });
                    // Created before process_document so a panic there still aborts the
                    // heartbeat via unwind; explicitly dropped on the normal path below.
                    let heartbeat_guard = AbortOnDrop(heartbeat);
                    let process_result = engine.process_document(&doc_id, &lease_token).await;
                    drop(heartbeat_guard);
                    if let Err(error) = process_result {
                        tracing::error!(doc_id, %error, "ingest failed");
                        let _ = engine
                            .db
                            .mark_document_failed(&doc_id, &lease_token, &error.to_string())
                            .await;
                    }
                    drop(permit);
                    engine.notify_dispatcher();
                });
            }
            tokio::select! {
                _ = poll.tick() => {},
                message = rx.recv() => {
                    if message.is_none() {
                        break;
                    }
                }
            }
        }
    }

    fn notify_dispatcher(&self) {
        let _ = self.tx.try_send(());
    }

    /// Accept content instantly: persist a queued document and enqueue it.
    pub async fn ingest(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        req: &IngestRequest,
    ) -> Result<(String, DocumentStatus)> {
        self.ingest_document(org_id, user_id, req, None, "api", None)
            .await
    }

    /// Ingest through a restricted caller, enforcing its container allowlist again
    /// at the custom-id upsert boundary to prevent authorization races.
    pub async fn ingest_scoped(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        req: &IngestRequest,
        allowed_tags: Option<&[String]>,
    ) -> Result<(String, DocumentStatus)> {
        self.ingest_document(org_id, user_id, req, None, "api", allowed_tags)
            .await
    }

    /// Ingest attributed to a connector source (sets `connection_id` + `source`).
    pub async fn ingest_from_connection(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        req: &IngestRequest,
        connection_id: &str,
        source: &str,
    ) -> Result<(String, DocumentStatus)> {
        self.ingest_document(org_id, user_id, req, Some(connection_id), source, None)
            .await
    }

    async fn ingest_document(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        req: &IngestRequest,
        connection_id: Option<&str>,
        source: &str,
        allowed_tags: Option<&[String]>,
    ) -> Result<(String, DocumentStatus)> {
        if req.content.trim().is_empty() {
            return Err(Error::BadRequest("content must not be empty".into()));
        }
        if req.content.len() > MAX_DOCUMENT_BYTES {
            return Err(Error::BadRequest("content exceeds 10 MiB".into()));
        }
        if req.custom_id.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > 255 || value.chars().any(char::is_control)
        }) {
            return Err(Error::BadRequest(
                "customId must be 1..=255 printable characters".into(),
            ));
        }
        if req.title.as_ref().is_some_and(|value| value.len() > 512) {
            return Err(Error::BadRequest("title exceeds 512 bytes".into()));
        }
        if req
            .entity_context
            .as_ref()
            .is_some_and(|value| value.len() > 10 * 1024)
        {
            return Err(Error::BadRequest("entityContext exceeds 10 KiB".into()));
        }
        if req
            .content_type
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 64)
        {
            return Err(Error::BadRequest("contentType must be 1..=64 bytes".into()));
        }
        if req
            .raw
            .as_ref()
            .is_some_and(|value| value.len() > MAX_DOCUMENT_BYTES)
        {
            return Err(Error::BadRequest("raw content exceeds 10 MiB".into()));
        }
        if let Some(metadata) = &req.metadata {
            validate_metadata(metadata)?;
        }
        let mut tags = req.resolved_container_tags();
        if tags.is_empty() {
            tags.push(memoricai_core::DEFAULT_CONTAINER_TAG.to_string());
        }
        if tags.len() > 20 {
            return Err(Error::BadRequest(
                "at most 20 container tags are allowed".into(),
            ));
        }
        let mut unique_tags = std::collections::HashSet::with_capacity(tags.len());
        for t in &tags {
            if !memoricai_core::is_valid_container_tag(t) {
                return Err(Error::BadRequest(format!("invalid container tag: {t}")));
            }
            if !unique_tags.insert(t) {
                return Err(Error::BadRequest(format!("duplicate container tag: {t}")));
            }
            if allowed_tags.is_some_and(|allowed| !allowed.iter().any(|tag| tag == t)) {
                return Err(Error::Forbidden(
                    "container tag outside credential scope".into(),
                ));
            }
        }
        for tag in &tags {
            self.db.ensure_space(org_id, tag, user_id).await?;
        }

        let content_hash = blake3::hash(req.content.as_bytes()).to_hex().to_string();
        let doc_type = req
            .content_type
            .clone()
            .unwrap_or_else(|| extract::detect_type(&req.content, req.title.as_deref()));
        let metadata = req
            .metadata
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
        let url = if doc_type == "webpage" {
            Some(req.content.clone())
        } else {
            None
        };
        let now: Timestamp = chrono::Utc::now();

        // Dedup / upsert by customId.
        if let Some(cid) = &req.custom_id {
            if let Some(existing) = self.db.find_document_by_custom_id(org_id, cid).await? {
                if allowed_tags.is_some_and(|allowed| {
                    existing
                        .container_tags
                        .iter()
                        .any(|tag| !allowed.iter().any(|candidate| candidate == tag))
                }) {
                    return Err(Error::Forbidden(
                        "customId identifies a document shared with an unauthorized container"
                            .into(),
                    ));
                }
                let unchanged = existing.content_hash.as_deref() == Some(content_hash.as_str())
                    && existing.title == req.title
                    && existing.doc_type == doc_type
                    && existing.container_tags == tags
                    && existing.connection_id.as_deref() == connection_id
                    && existing.source.as_deref() == Some(source)
                    && existing.url == url
                    && existing.metadata == metadata;
                if unchanged {
                    return Ok((existing.id, existing.status)); // unchanged
                }
                self.db
                    .queue_document_replacement(
                        org_id,
                        &existing.id,
                        &req.content,
                        &content_hash,
                        &metadata,
                        req.title.as_deref(),
                        &doc_type,
                        &tags,
                        connection_id,
                        source,
                        url.as_deref(),
                        allowed_tags,
                    )
                    .await?;
                self.notify_dispatcher();
                return Ok((existing.id, DocumentStatus::Queued));
            }
        }

        let doc = Document {
            id: memoricai_core::ids::document_id(),
            custom_id: req.custom_id.clone(),
            content_hash: Some(content_hash),
            org_id: org_id.to_string(),
            user_id: user_id.map(|s| s.to_string()),
            connection_id: connection_id.map(|s| s.to_string()),
            title: req.title.clone(),
            summary: None,
            content: Some(req.content.clone()),
            url,
            source: Some(source.to_string()),
            doc_type,
            status: DocumentStatus::Queued,
            metadata,
            container_tags: tags,
            token_count: None,
            chunk_count: Some(0),
            created_at: now,
            updated_at: now,
        };
        self.db.insert_document(&doc, req.raw.as_deref()).await?;
        self.notify_dispatcher();
        Ok((doc.id, DocumentStatus::Queued))
    }

    /// Update a document and schedule reindexing when its content changes.
    pub async fn patch_document(
        &self,
        org_id: &str,
        id: &str,
        content: Option<&str>,
        metadata: Option<&serde_json::Value>,
        allowed_tags: Option<&[String]>,
    ) -> Result<Document> {
        if let Some(content) = content {
            if content.trim().is_empty() {
                return Err(Error::BadRequest("content must not be empty".into()));
            }
            if content.len() > MAX_DOCUMENT_BYTES {
                return Err(Error::BadRequest("content exceeds 10 MiB".into()));
            }
        }
        if let Some(metadata) = metadata {
            validate_metadata(metadata)?;
        }
        let document = match content {
            Some(content) => {
                let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                self.db
                    .queue_document_update(
                        org_id,
                        id,
                        content,
                        &content_hash,
                        metadata,
                        allowed_tags,
                    )
                    .await?
            }
            None => {
                self.db
                    .patch_document(org_id, id, None, metadata, allowed_tags)
                    .await?
            }
        };
        if content.is_some() {
            self.notify_dispatcher();
        }
        Ok(document)
    }

    /// Directly create memories, bypassing the ingestion pipeline (`POST /v1/memories`).
    pub async fn create_memories(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        req: &CreateMemoriesRequest,
    ) -> Result<CreateMemoriesResponse> {
        if !memoricai_core::is_valid_container_tag(&req.container_tag) {
            return Err(Error::BadRequest("invalid container tag".into()));
        }
        if req.memories.is_empty() || req.memories.len() > 100 {
            return Err(Error::BadRequest(
                "memories must contain between 1 and 100 items".into(),
            ));
        }
        for memory in &req.memories {
            if memory.content.trim().is_empty() || memory.content.len() > MAX_MEMORY_BYTES {
                return Err(Error::BadRequest(
                    "each memory must contain 1..=10240 bytes".into(),
                ));
            }
            if let Some(metadata) = &memory.metadata {
                validate_metadata(metadata)?;
            }
        }
        self.db
            .ensure_space(org_id, &req.container_tag, user_id)
            .await?;

        let texts: Vec<String> = req.memories.iter().map(|m| m.content.clone()).collect();
        let embeddings = self.models.embedder.embed_batch(&texts).await?;
        validate_embedding_batch(&texts, &embeddings, self.models.dim())?;
        let now: Timestamp = chrono::Utc::now();
        let mut created = Vec::with_capacity(req.memories.len());
        for (input, emb) in req.memories.iter().zip(embeddings.iter()) {
            let mem = Memory {
                id: memoricai_core::ids::memory_id(),
                custom_id: None,
                document_id: None,
                org_id: org_id.to_string(),
                user_id: user_id.map(|s| s.to_string()),
                memory: input.content.clone(),
                summary: None,
                mem_type: None,
                space_container_tag: req.container_tag.clone(),
                version: 1,
                is_latest: true,
                parent_memory_id: None,
                root_memory_id: None,
                relation: None,
                source_count: 1,
                is_static: input.is_static,
                is_inference: false,
                review_status: None,
                is_forgotten: false,
                forget_reason: None,
                forget_after: None,
                forget_batch_id: None,
                event_date: None,
                metadata: input
                    .metadata
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({})),
                created_at: now,
                updated_at: now,
            };
            self.db.insert_memory(&mem, emb).await?;
            created.push(CreatedMemory {
                id: mem.id,
                memory: mem.memory,
                is_static: mem.is_static,
                created_at: now,
            });
        }
        Ok(CreateMemoriesResponse {
            document_id: None,
            memories: created,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_validation_rejects_malformed_provider_output() {
        let texts = vec!["one".to_string()];
        assert!(validate_embedding_batch(&texts, &[], 2).is_err());
        assert!(validate_embedding_batch(&texts, &[vec![1.0]], 2).is_err());
        assert!(validate_embedding_batch(&texts, &[vec![0.0, 0.0]], 2).is_err());
        assert!(validate_embedding_batch(&texts, &[vec![f32::NAN, 1.0]], 2).is_err());
        assert!(validate_embedding_batch(&texts, &[vec![0.5, -0.5]], 2).is_ok());
    }
}
