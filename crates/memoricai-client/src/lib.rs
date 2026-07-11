//! memoricai-client: Rust SDK for the memoricai `/v1` HTTP API.
//!
//! ```no_run
//! use memoricai_client::Client;
//!
//! # async fn demo() -> Result<(), memoricai_client::ClientError> {
//! let client = Client::new("http://localhost:6767", "mc_...");
//! let doc = client.add_text("My name is Ada.", "mc_project_default").await?;
//! client.wait_for_document(&doc.id, std::time::Duration::from_secs(60)).await?;
//! let res = client
//!     .search_memories(&memoricai_client::MemorySearchRequest {
//!         q: "what is my name".into(),
//!         container_tag: Some("mc_project_default".into()),
//!         digest: true,
//!         ..Default::default()
//!     })
//!     .await?;
//! println!("{:?}", res.digest);
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use memoricai_core::dto::{
    BulkDeleteResponse, CreateMemoriesRequest, CreateMemoriesResponse, DocumentListRequest,
    DocumentListResponse, DocumentSearchRequest, DocumentSearchResponse, ForgetMatchingRequest,
    ForgetMatchingResponse, ForgetRequest, IngestRequest, IngestResponse, MemoryInput,
    MemorySearchRequest, MemorySearchResponse, PatchMemoryRequest, ProfileRequest, ProfileResponse,
    SearchInclude,
};
pub use memoricai_core::model::{Document, Memory, Profile};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("api error {status}: {message}")]
    Api { status: u16, message: String },
    #[error("document {0} failed processing: {1}")]
    ProcessingFailed(String, String),
    #[error("timed out waiting for document {0}")]
    Timeout(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Client for a memoricai server's `/v1` API.
#[derive(Clone)]
pub struct Client {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

#[derive(serde::Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl Client {
    /// `base_url` like `http://localhost:6767`; `api_key` an `mc_...` key.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    async fn request<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<R> {
        let mut req = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .bearer_auth(&self.api_key);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let message = serde_json::from_str::<ApiErrorBody>(&text)
                .ok()
                .and_then(|e| e.message.or(e.error))
                .unwrap_or(text);
            return Err(ClientError::Api {
                status: status.as_u16(),
                message,
            });
        }
        Ok(resp.json().await?)
    }

    /// `GET /health`.
    pub async fn health(&self) -> Result<serde_json::Value> {
        self.request::<(), _>(reqwest::Method::GET, "/health", None)
            .await
    }

    // ---------------- documents ----------------

    /// `POST /v1/documents` — accept content for ingestion (returns instantly
    /// with status `queued`; processing is asynchronous).
    pub async fn add_document(&self, req: &IngestRequest) -> Result<IngestResponse> {
        self.request(reqwest::Method::POST, "/v1/documents", Some(req))
            .await
    }

    /// Convenience: ingest plain text into a container tag.
    pub async fn add_text(
        &self,
        content: impl Into<String>,
        container_tag: impl Into<String>,
    ) -> Result<IngestResponse> {
        self.add_document(&IngestRequest {
            content: content.into(),
            custom_id: None,
            container_tag: Some(container_tag.into()),
            container_tags: None,
            metadata: None,
            entity_context: None,
            content_type: None,
            title: None,
            raw: None,
        })
        .await
    }

    /// `GET /v1/documents/{id}`.
    pub async fn get_document(&self, id: &str) -> Result<Document> {
        self.request::<(), _>(reqwest::Method::GET, &format!("/v1/documents/{id}"), None)
            .await
    }

    /// `DELETE /v1/documents/{id}`.
    pub async fn delete_document(&self, id: &str) -> Result<serde_json::Value> {
        self.request::<(), _>(
            reqwest::Method::DELETE,
            &format!("/v1/documents/{id}"),
            None,
        )
        .await
    }

    /// `POST /v1/documents/list`.
    pub async fn list_documents(&self, req: &DocumentListRequest) -> Result<DocumentListResponse> {
        self.request(reqwest::Method::POST, "/v1/documents/list", Some(req))
            .await
    }

    /// `POST /v1/documents/search` — chunk-level RAG over documents.
    pub async fn search_documents(
        &self,
        req: &DocumentSearchRequest,
    ) -> Result<DocumentSearchResponse> {
        self.request(reqwest::Method::POST, "/v1/documents/search", Some(req))
            .await
    }

    /// Poll `GET /v1/documents/{id}` until it reaches `done` (or fail/timeout).
    pub async fn wait_for_document(&self, id: &str, timeout: Duration) -> Result<Document> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let doc = self.get_document(id).await?;
            match doc.status {
                memoricai_core::enums::DocumentStatus::Done => return Ok(doc),
                memoricai_core::enums::DocumentStatus::Failed => {
                    return Err(ClientError::ProcessingFailed(
                        id.to_string(),
                        "document status is failed".into(),
                    ))
                }
                _ if std::time::Instant::now() >= deadline => {
                    return Err(ClientError::Timeout(id.to_string()))
                }
                _ => tokio::time::sleep(Duration::from_millis(400)).await,
            }
        }
    }

    // ---------------- search / profile ----------------

    /// `POST /v1/search` — memory-graph search. Set `digest: true` in the
    /// request to receive a compact, date-stamped context digest.
    pub async fn search_memories(&self, req: &MemorySearchRequest) -> Result<MemorySearchResponse> {
        self.request(reqwest::Method::POST, "/v1/search", Some(req))
            .await
    }

    /// `POST /v1/profile` — static/dynamic/bucketed user profile.
    pub async fn profile(&self, req: &ProfileRequest) -> Result<ProfileResponse> {
        self.request(reqwest::Method::POST, "/v1/profile", Some(req))
            .await
    }

    // ---------------- memories ----------------

    /// `POST /v1/memories` — create memories directly (no extraction).
    pub async fn create_memories(
        &self,
        req: &CreateMemoriesRequest,
    ) -> Result<CreateMemoriesResponse> {
        self.request(reqwest::Method::POST, "/v1/memories", Some(req))
            .await
    }

    /// `PATCH /v1/memories` — versioned update of a memory.
    pub async fn patch_memory(&self, req: &PatchMemoryRequest) -> Result<Memory> {
        self.request(reqwest::Method::PATCH, "/v1/memories", Some(req))
            .await
    }

    /// `DELETE /v1/memories` — forget one memory by id or exact content.
    pub async fn forget_memory(&self, req: &ForgetRequest) -> Result<Memory> {
        self.request(reqwest::Method::DELETE, "/v1/memories", Some(req))
            .await
    }

    /// `POST /v1/memories/forget-matching` — semantic bulk forget.
    pub async fn forget_matching(
        &self,
        req: &ForgetMatchingRequest,
    ) -> Result<ForgetMatchingResponse> {
        self.request(
            reqwest::Method::POST,
            "/v1/memories/forget-matching",
            Some(req),
        )
        .await
    }
}
