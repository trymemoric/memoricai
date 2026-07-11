//! Provider ports: the pluggable model layer the engine depends on. Used as
//! trait objects (`Arc<dyn ...>`), so all methods are object-safe.

use crate::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Request a JSON-object response (provider `response_format`).
    pub json: bool,
}

/// Marker the extraction prompt places before raw content, letting providers
/// (and the deterministic test fake) locate the content in the prompt.
pub const CONTENT_MARKER: &str = "CONTENT:";

/// A text-generation provider (OpenAI-compatible, Anthropic, Gemini…).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, messages: Vec<ChatMessage>, opts: ChatOptions) -> Result<String>;
    /// Human-readable provider label (for diagnostics).
    fn label(&self) -> &str {
        "llm"
    }
}

/// An embedding provider. Vectors MUST be returned L2-normalized.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn dim(&self) -> usize;
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self
            .embed_batch(std::slice::from_ref(&text.to_string()))
            .await?;
        v.pop()
            .ok_or_else(|| crate::error::Error::Model("empty embedding response".into()))
    }
}

/// A cross-encoder reranker. Returns one score per passage (higher = better).
#[async_trait]
pub trait Reranker: Send + Sync {
    async fn rerank(&self, query: &str, passages: &[String]) -> Result<Vec<f32>>;
}

/// Speech-to-text for audio/video ingestion.
#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, audio: &[u8], filename: &str, mime: &str) -> Result<String>;
}

/// Image understanding (OCR + captioning) via a multimodal model.
#[async_trait]
pub trait Vision: Send + Sync {
    async fn caption(&self, image: &[u8], mime: &str, prompt: &str) -> Result<String>;
}

/// L2-normalize a vector in place (so cosine == dot product).
pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
