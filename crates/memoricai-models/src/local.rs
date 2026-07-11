//! In-process embedding generation via fastembed (ONNX). Compiled only with
//! the `local-embeddings` feature; selected with
//! `MEMORICAI_EMBEDDING_PROVIDER=local`. Model weights download once into the
//! cache directory and run fully offline afterwards.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use memoricai_core::error::{Error, Result};
use memoricai_core::ports::{l2_normalize, EmbeddingProvider};

/// A supported local model: fastembed id, output dimension, and the task
/// prefixes its retrieval training expects (asymmetric models score queries
/// and passages differently).
struct ModelSpec {
    model: EmbeddingModel,
    dim: usize,
    doc_prefix: &'static str,
    query_prefix: &'static str,
}

fn spec_for(name: &str) -> Option<ModelSpec> {
    match name.to_lowercase().as_str() {
        "nomic-embed-text-v1.5" | "nomic-embed-text" => Some(ModelSpec {
            model: EmbeddingModel::NomicEmbedTextV15,
            dim: 768,
            doc_prefix: "search_document: ",
            query_prefix: "search_query: ",
        }),
        // Quantized variant: ~4x faster CPU ingestion, a fraction of the RAM,
        // marginal retrieval-quality cost. The practical default for
        // CPU-only deployments.
        "nomic-embed-text-v1.5-q" | "nomic-embed-text-q" => Some(ModelSpec {
            model: EmbeddingModel::NomicEmbedTextV15Q,
            dim: 768,
            doc_prefix: "search_document: ",
            query_prefix: "search_query: ",
        }),
        "bge-small-en-v1.5" => Some(ModelSpec {
            model: EmbeddingModel::BGESmallENV15,
            dim: 384,
            doc_prefix: "",
            query_prefix: "Represent this sentence for searching relevant passages: ",
        }),
        "all-minilm-l6-v2" => Some(ModelSpec {
            model: EmbeddingModel::AllMiniLML6V2,
            dim: 384,
            doc_prefix: "",
            query_prefix: "",
        }),
        _ => None,
    }
}

pub struct LocalEmbedder {
    // Small session pool: ONNX sessions are not shareable across threads
    // mid-inference, and a single mutex-serialized session leaves CPU idle
    // during concurrent ingestion.
    pool: Vec<Arc<Mutex<TextEmbedding>>>,
    next: std::sync::atomic::AtomicUsize,
    dim: usize,
    doc_prefix: &'static str,
    query_prefix: &'static str,
    label: String,
}

impl LocalEmbedder {
    /// Load (downloading on first use into `cache_dir`) the named model.
    /// Supported: `nomic-embed-text-v1.5` (768d, default),
    /// `bge-small-en-v1.5` (384d), `all-minilm-l6-v2` (384d).
    pub fn new(model_name: &str, cache_dir: PathBuf) -> Result<Self> {
        let spec = spec_for(model_name).ok_or_else(|| {
            Error::Model(format!(
                "unsupported local embedding model {model_name:?}; supported: \
                 nomic-embed-text-v1.5, nomic-embed-text-v1.5-q, \
                 bge-small-en-v1.5, all-minilm-l6-v2"
            ))
        })?;
        let pool_size: usize = std::env::var("MEMORICAI_LOCAL_EMBEDDING_POOL")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2)
            .clamp(1, 8);
        let mut pool = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let embedding = TextEmbedding::try_new(
                TextInitOptions::new(spec.model.clone()).with_cache_dir(cache_dir.clone()),
            )
            .map_err(|error| {
                Error::Model(format!("failed to load local embedding model: {error}"))
            })?;
            pool.push(Arc::new(Mutex::new(embedding)));
        }
        Ok(Self {
            pool,
            next: std::sync::atomic::AtomicUsize::new(0),
            dim: spec.dim,
            doc_prefix: spec.doc_prefix,
            query_prefix: spec.query_prefix,
            label: format!("local:{model_name}"),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    async fn embed_with_prefix(&self, texts: &[String], prefix: &str) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let inputs: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        let slot = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.pool.len();
        let inner = Arc::clone(&self.pool[slot]);
        let mut out = tokio::task::spawn_blocking(move || {
            let mut model = inner
                .lock()
                .map_err(|_| Error::Model("local embedder poisoned".into()))?;
            model
                .embed(inputs, None)
                .map_err(|error| Error::Model(format!("local embedding failed: {error}")))
        })
        .await
        .map_err(|error| Error::Model(format!("local embedding task failed: {error}")))??;
        for v in &mut out {
            l2_normalize(v);
        }
        Ok(out)
    }
}

#[async_trait]
impl EmbeddingProvider for LocalEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_with_prefix(texts, self.doc_prefix).await
    }

    async fn embed_query_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_with_prefix(texts, self.query_prefix).await
    }
}
