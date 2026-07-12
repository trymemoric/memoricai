//! Deterministic in-process providers for tests ONLY. Never used in
//! production: `ModelStack::from_env` requires real model configuration and
//! these types are reachable solely through `ModelStack::for_tests`.
//!
//! Embeddings use feature-hashing so lexically-similar texts have high cosine
//! similarity; the LLM performs sentence-splitting "extraction".

use async_trait::async_trait;
use memoricai_core::error::Result;
use memoricai_core::ports::{
    l2_normalize, ChatMessage, ChatOptions, EmbeddingProvider, LlmProvider, CONTENT_MARKER,
};

pub struct DeterministicLlm;

#[async_trait]
impl LlmProvider for DeterministicLlm {
    async fn complete(&self, messages: Vec<ChatMessage>, opts: ChatOptions) -> Result<String> {
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        if opts.json {
            let content = match last_user.find(CONTENT_MARKER) {
                Some(i) => last_user[i + CONTENT_MARKER.len()..].trim(),
                None => last_user.trim(),
            };
            let facts: Vec<String> = split_sentences(content);
            let mems: Vec<serde_json::Value> = facts
                .into_iter()
                .map(|f| {
                    let event_date = find_iso_date(&f);
                    serde_json::json!({
                        "content": f,
                        "isStatic": false,
                        "forgetAfter": null,
                        "eventDate": event_date,
                    })
                })
                .collect();
            Ok(serde_json::json!({"memories": mems}).to_string())
        } else {
            // Non-JSON: echo a short summary (first sentence).
            Ok(split_sentences(last_user)
                .into_iter()
                .next()
                .unwrap_or_default())
        }
    }
}

pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

#[async_trait]
impl EmbeddingProvider for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| feature_hash(t, self.dim)).collect())
    }
}

fn feature_hash(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; dim];
    for tok in text.split(|c: char| !c.is_alphanumeric()) {
        if tok.is_empty() {
            continue;
        }
        let tok = tok.to_lowercase();
        let h = fnv1a(tok.as_bytes()) as usize;
        let idx = h % dim;
        let sign = if (h >> 1) & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }
    // Ensure non-zero so normalization is well-defined.
    if v.iter().all(|x| *x == 0.0) {
        v[0] = 1.0;
    }
    l2_normalize(&mut v);
    v
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// First `YYYY-MM-DD` substring in the text, if any (fake "event date"
/// extraction so tests exercise the event_date pathway).
fn find_iso_date(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for start in 0..bytes.len().saturating_sub(9) {
        let candidate = &bytes[start..start + 10];
        let shaped = candidate.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        });
        if shaped {
            return std::str::from_utf8(candidate).ok().map(str::to_string);
        }
    }
    None
}

/// Split text into trimmed sentence-ish fragments, dropping very short ones.
pub fn split_sentences(text: &str) -> Vec<String> {
    text.split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|s| s.len() >= 3)
        .map(str::to_string)
        .collect()
}
