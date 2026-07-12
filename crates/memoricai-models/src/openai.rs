//! OpenAI-compatible provider — works against OpenAI, Ollama, LM Studio, vLLM,
//! Groq, Together, etc. (anything exposing `/chat/completions` + `/embeddings`).

use async_trait::async_trait;
use memoricai_core::error::{Error, Result};
use memoricai_core::ports::{
    l2_normalize, ChatMessage, ChatOptions, EmbeddingProvider, LlmProvider,
};
use serde_json::json;

fn client() -> reqwest::Client {
    static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client")
    });
    CLIENT.clone()
}

fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub struct OpenAiChat {
    base_url: String,
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl OpenAiChat {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            model: model.into(),
            http: client(),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiChat {
    async fn complete(&self, messages: Vec<ChatMessage>, opts: ChatOptions) -> Result<String> {
        let model = opts.model.as_deref().unwrap_or(&self.model);
        let mut body = json!({
            "model": model,
            "messages": messages,
        });
        if let Some(t) = opts.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = opts.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if opts.json {
            body["response_format"] = json!({"type": "json_object"});
        }

        let mut req = self
            .http
            .post(join(&self.base_url, "chat/completions"))
            .json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| Error::Model(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Model(format!("chat {status}: {text}")));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| Error::Model(e.to_string()))?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Model("no content in chat response".into()))
    }
}

/// OpenAI caps `/embeddings` at 2048 inputs (plus a per-request token budget); stay
/// well under both so a large document's chunks aren't sent as one oversized request
/// that the provider rejects with a 400.
const MAX_EMBED_INPUTS: usize = 128;

pub struct OpenAiEmbedder {
    base_url: String,
    api_key: Option<String>,
    model: String,
    dim: usize,
    http: reqwest::Client,
}

impl OpenAiEmbedder {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
        dim: usize,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            model: model.into(),
            dim,
            http: client(),
        }
    }

    /// Embed a single request's worth of inputs (caller must bound the count).
    async fn embed_request(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = json!({"model": self.model, "input": texts});
        let mut req = self
            .http
            .post(join(&self.base_url, "embeddings"))
            .json(&body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| Error::Model(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Model(format!("embeddings {status}: {text}")));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| Error::Model(e.to_string()))?;
        let data = v["data"]
            .as_array()
            .ok_or_else(|| Error::Model("no data in embeddings response".into()))?;
        if data.len() != texts.len() {
            return Err(Error::Model(format!(
                "embedding provider returned {} vectors for {} inputs",
                data.len(),
                texts.len()
            )));
        }
        let mut out: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for (position, item) in data.iter().enumerate() {
            let index = item["index"]
                .as_u64()
                .map(|value| value as usize)
                .unwrap_or(position);
            if index >= out.len() || out[index].is_some() {
                return Err(Error::Model(
                    "embedding response contains an invalid or duplicate index".into(),
                ));
            }
            let arr = item["embedding"]
                .as_array()
                .ok_or_else(|| Error::Model("embedding not an array".into()))?;
            if arr.len() != self.dim {
                return Err(Error::Model(format!(
                    "embedding dimension {} does not match configured dimension {}",
                    arr.len(),
                    self.dim
                )));
            }
            let mut vec = Vec::with_capacity(arr.len());
            for value in arr {
                let number = value
                    .as_f64()
                    .ok_or_else(|| Error::Model("embedding contains a non-number".into()))?;
                let number = number as f32;
                if !number.is_finite() {
                    return Err(Error::Model(
                        "embedding contains an invalid numeric value".into(),
                    ));
                }
                vec.push(number);
            }
            if vec.iter().map(|value| value * value).sum::<f32>() <= f32::EPSILON {
                return Err(Error::Model("embedding vector has zero magnitude".into()));
            }
            l2_normalize(&mut vec);
            out[index] = Some(vec);
        }
        out.into_iter()
            .map(|embedding| {
                embedding.ok_or_else(|| Error::Model("embedding response omitted an index".into()))
            })
            .collect()
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(MAX_EMBED_INPUTS) {
            out.extend(self.embed_request(batch).await?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const DIM: usize = 4;

    /// Read one HTTP request off `socket` and return the number of entries in its
    /// JSON body's `input` array.
    async fn read_input_count(socket: &mut tokio::net::TcpStream) -> usize {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        // Read until headers complete, then until the full Content-Length body arrives.
        let mut content_length: Option<usize> = None;
        let mut header_end: Option<usize> = None;
        loop {
            if header_end.is_none() {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    let headers = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                    for line in headers.lines() {
                        if let Some(v) = line.strip_prefix("content-length:") {
                            content_length = v.trim().parse().ok();
                        }
                    }
                }
            }
            if let (Some(he), Some(cl)) = (header_end, content_length) {
                if buf.len() >= he + cl {
                    let body = &buf[he..he + cl];
                    let v: serde_json::Value = serde_json::from_slice(body).unwrap();
                    return v["input"].as_array().map(|a| a.len()).unwrap_or(0);
                }
            }
            let n = socket.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        0
    }

    async fn respond(socket: &mut tokio::net::TcpStream, n: usize) {
        let data: Vec<serde_json::Value> = (0..n)
            .map(|i| json!({"index": i, "embedding": vec![0.5_f32; DIM]}))
            .collect();
        let body = serde_json::to_vec(&json!({ "data": data })).unwrap();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.flush().await.unwrap();
    }

    #[tokio::test]
    async fn embed_batch_splits_large_input_into_capped_requests() {
        // Bind first so connections queue even before `accept` is called.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let per_request: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = per_request.clone();

        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let count = read_input_count(&mut socket).await;
                seen.lock().unwrap().push(count);
                respond(&mut socket, count).await;
            }
        });

        let embedder = OpenAiEmbedder::new(format!("http://{addr}"), None, "test-model", DIM);
        let inputs: Vec<String> = (0..300).map(|i| format!("chunk {i}")).collect();
        let out = embedder.embed_batch(&inputs).await.unwrap();

        // Every input got exactly one vector back, in order.
        assert_eq!(out.len(), 300);
        assert!(out.iter().all(|v| v.len() == DIM));

        let counts = per_request.lock().unwrap().clone();
        // 300 inputs at a 128 cap => three requests of 128, 128, 44.
        assert_eq!(counts, vec![128, 128, 44]);
        assert!(
            counts.iter().all(|&c| c <= MAX_EMBED_INPUTS),
            "a request exceeded the input cap: {counts:?}"
        );
    }

    #[tokio::test]
    async fn embed_batch_empty_makes_no_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hit = Arc::new(Mutex::new(false));
        let flag = hit.clone();
        tokio::spawn(async move {
            let _ = listener.accept().await;
            *flag.lock().unwrap() = true;
        });
        let embedder = OpenAiEmbedder::new(format!("http://{addr}"), None, "m", DIM);
        let out = embedder.embed_batch(&[]).await.unwrap();
        assert!(out.is_empty());
        assert!(!*hit.lock().unwrap(), "empty input must not hit the network");
    }
}
