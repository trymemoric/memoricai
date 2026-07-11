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
    fn label(&self) -> &str {
        "openai-compatible"
    }

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
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
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
