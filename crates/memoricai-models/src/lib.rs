//! memoricai-models: the pluggable model layer. `ModelStack` bundles an LLM, an
//! embedder, and an optional reranker, built from env. Model configuration is
//! required; there is no implicit fallback provider.

pub mod extra;
pub mod openai;
#[doc(hidden)]
pub mod testing;

use std::sync::Arc;

use extra::{LlmReranker, OpenAiTranscriber, OpenAiVision, RemoteReranker};
use memoricai_core::error::{Error, Result};
use memoricai_core::ports::{EmbeddingProvider, LlmProvider, Reranker, Transcriber, Vision};
use openai::{OpenAiChat, OpenAiEmbedder};

pub struct ModelStack {
    pub llm: Arc<dyn LlmProvider>,
    pub embedder: Arc<dyn EmbeddingProvider>,
    pub reranker: Arc<dyn Reranker>,
    pub transcriber: Option<Arc<dyn Transcriber>>,
    pub vision: Option<Arc<dyn Vision>>,
    pub llm_label: String,
    pub embedder_label: String,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// First-of-list env lookup.
fn env_any(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| env(k))
}

impl ModelStack {
    /// Fully deterministic in-process stack for tests (`dim`-length hashed
    /// vectors, sentence-splitting extraction). Not reachable from `from_env`.
    pub fn for_tests(dim: usize) -> Self {
        let llm: Arc<dyn LlmProvider> = Arc::new(testing::DeterministicLlm);
        Self {
            reranker: Arc::new(LlmReranker::new(llm.clone())),
            llm,
            embedder: Arc::new(testing::HashEmbedder::new(dim)),
            transcriber: None,
            vision: None,
            llm_label: "test-deterministic".into(),
            embedder_label: "test-deterministic".into(),
        }
    }

    /// Build from environment. Chat and embedding endpoints are required —
    /// the server refuses to start without real model configuration.
    pub fn from_env() -> Result<Self> {
        let dim: usize = env_any(&["MEMORICAI_EMBEDDING_DIM"])
            .and_then(|s| s.parse().ok())
            .unwrap_or(1536);

        let llm_base =
            env_any(&["MEMORICAI_LLM_BASE_URL", "OPENAI_BASE_URL"]).ok_or_else(|| {
                Error::Model(
                "model configuration required: set MEMORICAI_LLM_BASE_URL (or OPENAI_BASE_URL) \
                 to an OpenAI-compatible chat endpoint"
                    .into(),
            )
            })?;
        let (llm, llm_label): (Arc<dyn LlmProvider>, String) = {
            let key = env_any(&["MEMORICAI_LLM_API_KEY", "OPENAI_API_KEY"]);
            let model = env_any(&["MEMORICAI_LLM_MODEL", "OPENAI_MODEL", "MEMORICAI_MODEL"])
                .unwrap_or_else(|| "gpt-4o-mini".into());
            (
                Arc::new(OpenAiChat::new(llm_base.clone(), key, model.clone())),
                format!("openai:{model}@{llm_base}"),
            )
        };

        let emb_base =
            env_any(&["MEMORICAI_EMBEDDING_BASE_URL", "OPENAI_BASE_URL"]).ok_or_else(|| {
                Error::Model(
                    "model configuration required: set MEMORICAI_EMBEDDING_BASE_URL (or \
                     OPENAI_BASE_URL) to an OpenAI-compatible embeddings endpoint"
                        .into(),
                )
            })?;
        let (embedder, embedder_label): (Arc<dyn EmbeddingProvider>, String) = {
            let key = env_any(&["MEMORICAI_EMBEDDING_API_KEY", "OPENAI_API_KEY"]);
            let model = env_any(&["MEMORICAI_EMBEDDING_MODEL"])
                .unwrap_or_else(|| "text-embedding-3-small".into());
            (
                Arc::new(OpenAiEmbedder::new(
                    emb_base.clone(),
                    key,
                    model.clone(),
                    dim,
                )),
                format!("openai:{model}@{emb_base}"),
            )
        };

        // Reranker: dedicated rerank endpoint if configured, else LLM-based.
        let reranker: Arc<dyn Reranker> = match env_any(&["MEMORICAI_RERANK_URL"]) {
            Some(url) => {
                let key = env_any(&["MEMORICAI_RERANK_API_KEY", "OPENAI_API_KEY"]);
                let model = env_any(&["MEMORICAI_RERANK_MODEL"]).unwrap_or_else(|| "rerank".into());
                Arc::new(RemoteReranker::new(url, key, model))
            }
            None => Arc::new(LlmReranker::new(llm.clone())),
        };

        // Transcription (audio/video) — optional.
        let transcriber: Option<Arc<dyn Transcriber>> =
            env_any(&["MEMORICAI_TRANSCRIBE_BASE_URL", "OPENAI_BASE_URL"]).map(|base| {
                let key = env_any(&["MEMORICAI_TRANSCRIBE_API_KEY", "OPENAI_API_KEY"]);
                let model =
                    env_any(&["MEMORICAI_TRANSCRIBE_MODEL"]).unwrap_or_else(|| "whisper-1".into());
                Arc::new(OpenAiTranscriber::new(&base, key, model)) as Arc<dyn Transcriber>
            });

        // Vision (image captioning / OCR) — optional.
        let vision: Option<Arc<dyn Vision>> = env_any(&["MEMORICAI_VISION_BASE_URL"]).map(|base| {
            let key = env_any(&["MEMORICAI_VISION_API_KEY", "OPENAI_API_KEY"]);
            let model =
                env_any(&["MEMORICAI_VISION_MODEL"]).unwrap_or_else(|| "gpt-4o-mini".into());
            Arc::new(OpenAiVision::new(&base, key, model)) as Arc<dyn Vision>
        });

        Ok(Self {
            llm,
            embedder,
            reranker,
            transcriber,
            vision,
            llm_label,
            embedder_label,
        })
    }

    pub fn dim(&self) -> usize {
        self.embedder.dim()
    }
}
