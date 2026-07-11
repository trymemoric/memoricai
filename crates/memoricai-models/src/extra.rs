//! Phase 2 model capabilities: reranking, transcription, and vision — all via
//! remote (OpenAI-compatible / TEI-style) endpoints or the existing LLM, so no
//! native ML dependencies are pulled in.

use async_trait::async_trait;
use memoricai_core::error::{Error, Result};
use memoricai_core::ports::{ChatMessage, ChatOptions, LlmProvider, Reranker, Transcriber, Vision};
use serde_json::json;
use std::sync::Arc;

fn client() -> reqwest::Client {
    static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("reqwest client")
    });
    CLIENT.clone()
}

// ---------------- reranking ----------------

/// Reranker backed by a TEI/Jina/Cohere-style `/rerank` endpoint.
pub struct RemoteReranker {
    url: String,
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl RemoteReranker {
    pub fn new(url: impl Into<String>, api_key: Option<String>, model: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            api_key,
            model: model.into(),
            http: client(),
        }
    }
}

#[async_trait]
impl Reranker for RemoteReranker {
    async fn rerank(&self, query: &str, passages: &[String]) -> Result<Vec<f32>> {
        if passages.is_empty() {
            return Ok(vec![]);
        }
        let body = json!({"model": self.model, "query": query, "documents": passages});
        let mut req = self.http.post(&self.url).json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| Error::Model(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(Error::Model(format!("rerank {}", resp.status())));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| Error::Model(e.to_string()))?;
        // Accept {results:[{index,relevance_score}]} (Cohere/Jina) or {scores:[..]} (TEI).
        let mut scores = vec![0f32; passages.len()];
        if let Some(results) = v["results"].as_array().or_else(|| v.as_array()) {
            let mut seen = vec![false; passages.len()];
            for r in results {
                let idx = r["index"]
                    .as_u64()
                    .ok_or_else(|| Error::Model("rerank result omitted index".into()))?
                    as usize;
                let s = r["relevance_score"]
                    .as_f64()
                    .or_else(|| r["score"].as_f64())
                    .ok_or_else(|| Error::Model("rerank result omitted score".into()))?
                    as f32;
                if idx >= scores.len() || seen[idx] || !s.is_finite() {
                    return Err(Error::Model("rerank result contains invalid data".into()));
                }
                scores[idx] = s;
                seen[idx] = true;
            }
            if seen.iter().any(|seen| !seen) {
                return Err(Error::Model("rerank response omitted a passage".into()));
            }
        } else if let Some(arr) = v["scores"].as_array() {
            if arr.len() != scores.len() {
                return Err(Error::Model(
                    "rerank response returned the wrong number of scores".into(),
                ));
            }
            for (i, s) in arr.iter().enumerate() {
                let score = s
                    .as_f64()
                    .ok_or_else(|| Error::Model("rerank score is not numeric".into()))?
                    as f32;
                if !score.is_finite() {
                    return Err(Error::Model("rerank score is not finite".into()));
                }
                scores[i] = score;
            }
        } else {
            return Err(Error::Model("unrecognized rerank response".into()));
        }
        Ok(scores)
    }
}

/// Fallback reranker that asks the LLM to score relevance 0..1 per passage.
pub struct LlmReranker {
    llm: Arc<dyn LlmProvider>,
}

impl LlmReranker {
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl Reranker for LlmReranker {
    async fn rerank(&self, query: &str, passages: &[String]) -> Result<Vec<f32>> {
        if passages.is_empty() {
            return Ok(vec![]);
        }
        let listing = passages
            .iter()
            .enumerate()
            .map(|(i, p)| format!("[{i}] {}", p.chars().take(500).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n");
        let system = ChatMessage::system(
            "Score how well each passage answers the query, 0.0-1.0. \
             Respond ONLY as JSON {\"scores\":[{\"index\":int,\"score\":number}]}.",
        );
        let user = ChatMessage::user(format!("Query: {query}\n\nPassages:\n{listing}"));
        let raw = self
            .llm
            .complete(
                vec![system, user],
                ChatOptions {
                    json: true,
                    temperature: Some(0.0),
                    ..Default::default()
                },
            )
            .await?;
        let v: serde_json::Value = serde_json::from_str(raw.trim())
            .map_err(|error| Error::Model(format!("invalid rerank response: {error}")))?;
        let entries = v["scores"]
            .as_array()
            .ok_or_else(|| Error::Model("rerank response omitted scores".into()))?;
        let mut scores = vec![0f32; passages.len()];
        let mut seen = vec![false; passages.len()];
        for entry in entries {
            let idx = entry["index"]
                .as_u64()
                .ok_or_else(|| Error::Model("rerank score omitted index".into()))?
                as usize;
            let score = entry["score"]
                .as_f64()
                .ok_or_else(|| Error::Model("rerank score is not numeric".into()))?
                as f32;
            if idx >= scores.len() || seen[idx] || !score.is_finite() {
                return Err(Error::Model("rerank result contains invalid data".into()));
            }
            scores[idx] = score;
            seen[idx] = true;
        }
        if seen.iter().any(|seen| !seen) {
            return Err(Error::Model("rerank response omitted a passage".into()));
        }
        Ok(scores)
    }
}

// ---------------- transcription ----------------

/// OpenAI-compatible `/audio/transcriptions` (Whisper-style) transcriber.
pub struct OpenAiTranscriber {
    url: String,
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl OpenAiTranscriber {
    pub fn new(base_url: &str, api_key: Option<String>, model: impl Into<String>) -> Self {
        Self {
            url: format!("{}/audio/transcriptions", base_url.trim_end_matches('/')),
            api_key,
            model: model.into(),
            http: client(),
        }
    }
}

#[async_trait]
impl Transcriber for OpenAiTranscriber {
    async fn transcribe(&self, audio: &[u8], filename: &str, mime: &str) -> Result<String> {
        let part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|e| Error::Model(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);
        let mut req = self.http.post(&self.url).multipart(form);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| Error::Model(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(Error::Model(format!("transcribe {}", resp.status())));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| Error::Model(e.to_string()))?;
        Ok(v["text"].as_str().unwrap_or_default().to_string())
    }
}

// ---------------- vision ----------------

/// Multimodal vision via an OpenAI-compatible chat endpoint (image_url data URI).
pub struct OpenAiVision {
    url: String,
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl OpenAiVision {
    pub fn new(base_url: &str, api_key: Option<String>, model: impl Into<String>) -> Self {
        Self {
            url: format!("{}/chat/completions", base_url.trim_end_matches('/')),
            api_key,
            model: model.into(),
            http: client(),
        }
    }
}

#[async_trait]
impl Vision for OpenAiVision {
    async fn caption(&self, image: &[u8], mime: &str, prompt: &str) -> Result<String> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(image);
        let data_uri = format!("data:{mime};base64,{b64}");
        let body = json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": data_uri}}
                ]
            }]
        });
        let mut req = self.http.post(&self.url).json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| Error::Model(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(Error::Model(format!("vision {}", resp.status())));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| Error::Model(e.to_string()))?;
        Ok(v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }
}
