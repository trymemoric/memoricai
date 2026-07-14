//! memoricai-client: Rust SDK for the memoricai `/v1` HTTP API.
//!
//! ```no_run
//! use memoricai_client::Client;
//!
//! # async fn demo() -> Result<(), memoricai_client::ClientError> {
//! let client = Client::new("http://localhost:7373", "mc_...");
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
use serde::{Deserialize, Serialize};

pub use memoricai_core::dto::{
    AnalyticsErrorsResponse, AnalyticsLogsResponse, AnalyticsUsageResponse, BatchIngestRequest,
    BatchIngestResponse, BucketsResponse, ConnectionListRequest, ContextDiagnostics,
    ContextEvidence, ContextOmission, ContextRequest, ContextResponse, CreateConnectionRequest,
    CreateConnectionResponse, CreateMemoriesRequest, CreateMemoriesResponse, CreateProjectRequest,
    CreateScopedKeyRequest, CreateScopedKeyResponse, DocumentListRequest, DocumentListResponse,
    DocumentSearchRequest, DocumentSearchResponse, ForgetMatchingRequest, ForgetMatchingResponse,
    ForgetRequest, InferredListResponse, IngestRequest, IngestResponse, MemoryInput,
    MemorySearchRequest, MemorySearchResponse, PatchMemoryRequest, ProfileRequest, ProfileResponse,
    ProjectDto, ProjectsResponse, RegisterClientRequest, RegisterClientResponse, ReviewRequest,
    SearchInclude, SessionResponse, TokenResponse, UpdateSettingsRequest,
};
pub use memoricai_core::model::{
    Connection, Document, Memory, OrgSettings, Profile, ProfileBucket, SyncRun,
};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchDocumentRequest {
    pub content: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkDeleteDocumentsRequest {
    pub ids: Option<Vec<String>>,
    pub container_tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProjectRequest {
    /// `delete` removes project data; `move` requires `target_project_id`.
    pub action: String,
    pub target_project_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateContainerTagRequest {
    pub name: Option<String>,
    pub entity_context: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProfileBucketRequest {
    pub container_tag: Option<String>,
    pub key: String,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub struct AnalyticsQuery {
    pub period: Option<String>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkDeleteDocumentsResponse {
    pub success: bool,
    pub deleted_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OAuthTokenRequest {
    pub grant_type: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
}

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

/// Percent-encode a single URL path segment (RFC 3986 unreserved set stays literal),
/// so a `customId` containing `/`, `?`, `#`, etc. does not break the request path.
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

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

async fn api_error(response: reqwest::Response) -> ClientError {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<ApiErrorBody>(&text)
        .ok()
        .and_then(|error| error.message.or(error.error))
        .unwrap_or(text);
    ClientError::Api { status, message }
}

fn analytics_path(resource: &str, query: &AnalyticsQuery) -> String {
    let mut parameters = Vec::new();
    if let Some(period) = &query.period {
        parameters.push(format!("period={}", encode_path_segment(period)));
    }
    if let Some(page) = query.page {
        parameters.push(format!("page={page}"));
    }
    if let Some(limit) = query.limit {
        parameters.push(format!("limit={limit}"));
    }
    if parameters.is_empty() {
        format!("/v1/analytics/{resource}")
    } else {
        format!("/v1/analytics/{resource}?{}", parameters.join("&"))
    }
}

/// Keep URL path separators readable while protecting characters (`?`, `#`)
/// that would otherwise be interpreted as part of the router request itself.
fn encode_router_target(target: &str) -> String {
    let mut encoded = String::with_capacity(target.len());
    for byte in target.bytes() {
        match byte {
            b'%' | b'?' | b'#' | b' ' | 0..=31 | 127..=255 => {
                encoded.push_str(&format!("%{byte:02X}"));
            }
            _ => encoded.push(byte as char),
        }
    }
    encoded
}

impl Client {
    /// `base_url` like `http://localhost:7373`; `api_key` an `mc_...` key.
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

    /// Send a JSON request to an API path and decode its JSON response.
    ///
    /// This low-level method is public as a forward-compatibility escape hatch
    /// for engine endpoints introduced after the SDK version in use.
    pub async fn request_json<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<R> {
        let url = format!("{}{}", self.base_url, path);
        const MAX_RETRIES: u32 = 4;
        for attempt in 0..=MAX_RETRIES {
            let mut req = self
                .http
                .request(method.clone(), url.as_str())
                .bearer_auth(&self.api_key);
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp.json().await?);
            }
            let code = status.as_u16();
            // Retry transient failures (429/5xx) with exponential backoff, matching the
            // Python and TypeScript clients.
            if (code == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
                tokio::time::sleep(Duration::from_millis(1000u64 << attempt)).await;
                continue;
            }
            let text = resp.text().await.unwrap_or_default();
            let message = serde_json::from_str::<ApiErrorBody>(&text)
                .ok()
                .and_then(|e| e.message.or(e.error))
                .unwrap_or(text);
            return Err(ClientError::Api {
                status: code,
                message,
            });
        }
        unreachable!("retry loop always returns")
    }

    /// `GET /health`.
    pub async fn health(&self) -> Result<serde_json::Value> {
        self.request_json::<(), _>(reqwest::Method::GET, "/health", None)
            .await
    }

    /// `GET /v1/openapi` — engine discovery document.
    pub async fn openapi(&self) -> Result<serde_json::Value> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/openapi", None)
            .await
    }

    pub async fn oauth_metadata(&self) -> Result<serde_json::Value> {
        self.request_json::<(), _>(
            reqwest::Method::GET,
            "/.well-known/oauth-authorization-server",
            None,
        )
        .await
    }

    pub async fn register_oauth_client(
        &self,
        req: &RegisterClientRequest,
    ) -> Result<RegisterClientResponse> {
        self.request_json(
            reqwest::Method::POST,
            "/api/auth/oauth2/register",
            Some(req),
        )
        .await
    }

    pub async fn exchange_oauth_token(&self, req: &OAuthTokenRequest) -> Result<TokenResponse> {
        const MAX_RETRIES: u32 = 4;
        for attempt in 0..=MAX_RETRIES {
            let response = self
                .http
                .post(format!("{}/api/auth/oauth2/token", self.base_url))
                .form(req)
                .send()
                .await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response.json().await?);
            }
            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
                tokio::time::sleep(Duration::from_millis(1000u64 << attempt)).await;
                continue;
            }
            return Err(api_error(response).await);
        }
        unreachable!("retry loop always returns")
    }

    // ---------------- documents ----------------

    /// `POST /v1/documents` — accept content for ingestion (returns instantly
    /// with status `queued`; processing is asynchronous).
    pub async fn add_document(&self, req: &IngestRequest) -> Result<IngestResponse> {
        self.request_json(reqwest::Method::POST, "/v1/documents", Some(req))
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

    /// `POST /v1/documents/batch` — enqueue up to 600 documents.
    pub async fn add_documents(&self, req: &BatchIngestRequest) -> Result<BatchIngestResponse> {
        self.request_json(reqwest::Method::POST, "/v1/documents/batch", Some(req))
            .await
    }

    /// `POST /v1/documents/file` — upload an extracted file as multipart form data.
    pub async fn upload_file(
        &self,
        bytes: &[u8],
        filename: &str,
        content_type: Option<&str>,
        container_tags: &[String],
        metadata: Option<&serde_json::Value>,
    ) -> Result<IngestResponse> {
        const MAX_RETRIES: u32 = 4;
        for attempt in 0..=MAX_RETRIES {
            let mut part =
                reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(filename.to_string());
            if let Some(content_type) = content_type {
                part = part.mime_str(content_type)?;
            }
            let mut form = reqwest::multipart::Form::new().part("file", part);
            for tag in container_tags {
                form = form.text("containerTags", tag.clone());
            }
            if let Some(metadata) = metadata {
                form = form.text("metadata", metadata.to_string());
            }
            let response = self
                .http
                .post(format!("{}/v1/documents/file", self.base_url))
                .bearer_auth(&self.api_key)
                .multipart(form)
                .send()
                .await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response.json().await?);
            }
            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
                tokio::time::sleep(Duration::from_millis(1000u64 << attempt)).await;
                continue;
            }
            return Err(api_error(response).await);
        }
        unreachable!("retry loop always returns")
    }

    /// `GET /v1/documents/{id}`.
    pub async fn get_document(&self, id: &str) -> Result<Document> {
        self.request_json::<(), _>(
            reqwest::Method::GET,
            &format!("/v1/documents/{}", encode_path_segment(id)),
            None,
        )
        .await
    }

    /// `DELETE /v1/documents/{id}`.
    pub async fn delete_document(&self, id: &str) -> Result<serde_json::Value> {
        self.request_json::<(), _>(
            reqwest::Method::DELETE,
            &format!("/v1/documents/{}", encode_path_segment(id)),
            None,
        )
        .await
    }

    /// `PATCH /v1/documents/{id}` — replace content and/or metadata, then reprocess.
    pub async fn patch_document(&self, id: &str, req: &PatchDocumentRequest) -> Result<Document> {
        self.request_json(
            reqwest::Method::PATCH,
            &format!("/v1/documents/{}", encode_path_segment(id)),
            Some(req),
        )
        .await
    }

    /// `POST /v1/documents/list`.
    pub async fn list_documents(&self, req: &DocumentListRequest) -> Result<DocumentListResponse> {
        self.request_json(reqwest::Method::POST, "/v1/documents/list", Some(req))
            .await
    }

    /// `POST /v1/documents/documents` — list documents with their memory entries.
    pub async fn list_documents_with_memories(
        &self,
        req: &DocumentListRequest,
    ) -> Result<serde_json::Value> {
        self.request_json(reqwest::Method::POST, "/v1/documents/documents", Some(req))
            .await
    }

    /// `GET /v1/documents/processing`.
    pub async fn list_processing_documents(&self) -> Result<serde_json::Value> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/documents/processing", None)
            .await
    }

    /// `DELETE /v1/documents/bulk` — delete by ids or container tags.
    pub async fn bulk_delete_documents(
        &self,
        req: &BulkDeleteDocumentsRequest,
    ) -> Result<BulkDeleteDocumentsResponse> {
        self.request_json(reqwest::Method::DELETE, "/v1/documents/bulk", Some(req))
            .await
    }

    /// `POST /v1/documents/search` — chunk-level RAG over documents.
    pub async fn search_documents(
        &self,
        req: &DocumentSearchRequest,
    ) -> Result<DocumentSearchResponse> {
        self.request_json(reqwest::Method::POST, "/v1/documents/search", Some(req))
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
        self.request_json(reqwest::Method::POST, "/v1/search", Some(req))
            .await
    }

    /// `POST /v1/context` — bounded, source-aware context ready for an LLM prompt.
    pub async fn build_context(&self, req: &ContextRequest) -> Result<ContextResponse> {
        self.request_json(reqwest::Method::POST, "/v1/context", Some(req))
            .await
    }

    /// `POST /v1/profile` — static/dynamic/bucketed user profile.
    pub async fn profile(&self, req: &ProfileRequest) -> Result<ProfileResponse> {
        self.request_json(reqwest::Method::POST, "/v1/profile", Some(req))
            .await
    }

    // ---------------- memories ----------------

    /// `POST /v1/memories` — create memories directly (no extraction).
    pub async fn create_memories(
        &self,
        req: &CreateMemoriesRequest,
    ) -> Result<CreateMemoriesResponse> {
        self.request_json(reqwest::Method::POST, "/v1/memories", Some(req))
            .await
    }

    /// `PATCH /v1/memories` — versioned update of a memory.
    pub async fn patch_memory(&self, req: &PatchMemoryRequest) -> Result<Memory> {
        self.request_json(reqwest::Method::PATCH, "/v1/memories", Some(req))
            .await
    }

    /// `DELETE /v1/memories` — forget one memory by id or exact content.
    pub async fn forget_memory(&self, req: &ForgetRequest) -> Result<Memory> {
        self.request_json(reqwest::Method::DELETE, "/v1/memories", Some(req))
            .await
    }

    /// `POST /v1/memories/forget-matching` — semantic bulk forget.
    pub async fn forget_matching(
        &self,
        req: &ForgetMatchingRequest,
    ) -> Result<ForgetMatchingResponse> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/memories/forget-matching",
            Some(req),
        )
        .await
    }

    // ---------------- projects / container tags ----------------

    pub async fn list_projects(&self) -> Result<ProjectsResponse> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/projects", None)
            .await
    }

    pub async fn list_container_tags(&self) -> Result<ProjectsResponse> {
        self.list_projects().await
    }

    pub async fn create_project(&self, req: &CreateProjectRequest) -> Result<ProjectDto> {
        self.request_json(reqwest::Method::POST, "/v1/projects", Some(req))
            .await
    }

    pub async fn delete_project(
        &self,
        id: &str,
        req: &DeleteProjectRequest,
    ) -> Result<serde_json::Value> {
        self.request_json(
            reqwest::Method::DELETE,
            &format!("/v1/projects/{}", encode_path_segment(id)),
            Some(req),
        )
        .await
    }

    pub async fn update_container_tag(
        &self,
        tag: &str,
        req: &UpdateContainerTagRequest,
    ) -> Result<ProjectDto> {
        self.request_json(
            reqwest::Method::PATCH,
            &format!("/v1/container-tags/{}", encode_path_segment(tag)),
            Some(req),
        )
        .await
    }

    pub async fn delete_container_tag(&self, tag: &str) -> Result<serde_json::Value> {
        self.request_json::<(), _>(
            reqwest::Method::DELETE,
            &format!("/v1/container-tags/{}", encode_path_segment(tag)),
            None,
        )
        .await
    }

    // ---------------- settings / auth ----------------

    pub async fn get_settings(&self) -> Result<OrgSettings> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/settings", None)
            .await
    }

    pub async fn update_settings(&self, req: &UpdateSettingsRequest) -> Result<OrgSettings> {
        self.request_json(reqwest::Method::PATCH, "/v1/settings", Some(req))
            .await
    }

    pub async fn reset_settings(&self) -> Result<serde_json::Value> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/settings/reset",
            Some(&serde_json::json!({ "confirmation": "RESET" })),
        )
        .await
    }

    pub async fn session(&self) -> Result<SessionResponse> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/session", None)
            .await
    }

    pub async fn create_scoped_key(
        &self,
        req: &CreateScopedKeyRequest,
    ) -> Result<CreateScopedKeyResponse> {
        self.request_json(reqwest::Method::POST, "/v1/auth/scoped-key", Some(req))
            .await
    }

    pub async fn revoke_scoped_key(&self, id: &str) -> Result<serde_json::Value> {
        self.request_json::<(), _>(
            reqwest::Method::DELETE,
            &format!("/v1/auth/scoped-key/{}", encode_path_segment(id)),
            None,
        )
        .await
    }

    // ---------------- profile buckets / inferred memories ----------------

    pub async fn list_profile_buckets(
        &self,
        container_tag: Option<&str>,
    ) -> Result<BucketsResponse> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/profile/buckets",
            Some(&serde_json::json!({ "containerTag": container_tag })),
        )
        .await
    }

    pub async fn create_profile_bucket(
        &self,
        req: &CreateProfileBucketRequest,
    ) -> Result<ProfileBucket> {
        self.request_json(reqwest::Method::POST, "/v1/buckets", Some(req))
            .await
    }

    pub async fn list_inferred_memories(&self, tag: &str) -> Result<InferredListResponse> {
        self.request_json::<(), _>(
            reqwest::Method::GET,
            &format!("/v1/container-tags/{}/inferred", encode_path_segment(tag)),
            None,
        )
        .await
    }

    pub async fn review_inferred_memory(
        &self,
        tag: &str,
        memory_id: &str,
        req: &ReviewRequest,
    ) -> Result<serde_json::Value> {
        self.request_json(
            reqwest::Method::POST,
            &format!(
                "/v1/container-tags/{}/inferred/{}/review",
                encode_path_segment(tag),
                encode_path_segment(memory_id)
            ),
            Some(req),
        )
        .await
    }

    // ---------------- analytics ----------------

    pub async fn analytics_usage(&self, query: &AnalyticsQuery) -> Result<AnalyticsUsageResponse> {
        self.request_json::<(), _>(reqwest::Method::GET, &analytics_path("usage", query), None)
            .await
    }

    pub async fn analytics_errors(
        &self,
        query: &AnalyticsQuery,
    ) -> Result<AnalyticsErrorsResponse> {
        self.request_json::<(), _>(reqwest::Method::GET, &analytics_path("errors", query), None)
            .await
    }

    pub async fn analytics_logs(&self, query: &AnalyticsQuery) -> Result<AnalyticsLogsResponse> {
        self.request_json::<(), _>(reqwest::Method::GET, &analytics_path("logs", query), None)
            .await
    }

    pub async fn analytics_memory(&self) -> Result<serde_json::Value> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/analytics/memory", None)
            .await
    }

    pub async fn analytics_chat(&self) -> Result<serde_json::Value> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/analytics/chat", None)
            .await
    }

    // ---------------- connections ----------------

    pub async fn list_connections(&self) -> Result<Vec<Connection>> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/connections", None)
            .await
    }

    pub async fn filter_connections(&self, req: &ConnectionListRequest) -> Result<Vec<Connection>> {
        self.request_json(reqwest::Method::POST, "/v1/connections/list", Some(req))
            .await
    }

    pub async fn create_connection(
        &self,
        provider: &str,
        req: &CreateConnectionRequest,
    ) -> Result<CreateConnectionResponse> {
        self.request_json(
            reqwest::Method::POST,
            &format!("/v1/connections/{}", encode_path_segment(provider)),
            Some(req),
        )
        .await
    }

    pub async fn get_connection(&self, id: &str) -> Result<Connection> {
        self.request_json::<(), _>(
            reqwest::Method::GET,
            &format!("/v1/connections/{}", encode_path_segment(id)),
            None,
        )
        .await
    }

    pub async fn delete_connection(
        &self,
        id_or_provider: &str,
        delete_documents: bool,
    ) -> Result<serde_json::Value> {
        self.request_json::<(), _>(
            reqwest::Method::DELETE,
            &format!(
                "/v1/connections/{}?deleteDocuments={delete_documents}",
                encode_path_segment(id_or_provider)
            ),
            None,
        )
        .await
    }

    pub async fn import_connection(&self, id_or_provider: &str) -> Result<serde_json::Value> {
        self.request_json(
            reqwest::Method::POST,
            &format!(
                "/v1/connections/{}/import",
                encode_path_segment(id_or_provider)
            ),
            Some(&serde_json::json!({})),
        )
        .await
    }

    pub async fn connection_sync_runs(&self, id: &str) -> Result<Vec<SyncRun>> {
        self.request_json::<(), _>(
            reqwest::Method::GET,
            &format!("/v1/connections/{}/sync-runs", encode_path_segment(id)),
            None,
        )
        .await
    }

    pub async fn connection_resources(
        &self,
        id: &str,
        page: u32,
        per_page: u32,
    ) -> Result<serde_json::Value> {
        self.request_json::<(), _>(
            reqwest::Method::GET,
            &format!(
                "/v1/connections/{}/resources?page={page}&perPage={per_page}",
                encode_path_segment(id)
            ),
            None,
        )
        .await
    }

    pub async fn configure_connection(
        &self,
        id: &str,
        configuration: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.request_json(
            reqwest::Method::POST,
            &format!("/v1/connections/{}/configure", encode_path_segment(id)),
            Some(configuration),
        )
        .await
    }

    // ---------------- memory router / MCP OAuth helpers ----------------

    /// Proxy an OpenAI-compatible request and return the raw response so SSE
    /// streaming bodies remain available to the caller.
    pub async fn router_request<B: Serialize + ?Sized>(
        &self,
        upstream_url: &str,
        body: &B,
        upstream_api_key: &str,
        container_tag: Option<&str>,
    ) -> Result<reqwest::Response> {
        const MAX_RETRIES: u32 = 4;
        let url = format!(
            "{}/v1/router/{}",
            self.base_url,
            encode_router_target(upstream_url)
        );
        for attempt in 0..=MAX_RETRIES {
            let mut request = self
                .http
                .post(&url)
                .bearer_auth(upstream_api_key)
                .header("x-memoricai-api-key", &self.api_key)
                .json(body);
            if let Some(container_tag) = container_tag {
                request = request.header("x-mc-project", container_tag);
            }
            let response = request.send().await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
                tokio::time::sleep(Duration::from_millis(1000u64 << attempt)).await;
                continue;
            }
            return Err(api_error(response).await);
        }
        unreachable!("retry loop always returns")
    }

    pub async fn mcp_session_with_key(&self) -> Result<serde_json::Value> {
        self.request_json::<(), _>(reqwest::Method::GET, "/v1/mcp/session-with-key", None)
            .await
    }

    pub async fn connect_mcp_scope(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        self.request_json(reqwest::Method::POST, "/v1/mcp/connect-scope", Some(body))
            .await
    }

    /// Control-plane provisioning. Construct this client with the provision
    /// key, not an organization key.
    pub async fn provision(&self, org_name: &str, email: &str) -> Result<serde_json::Value> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/admin/provision",
            Some(&serde_json::json!({ "orgName": org_name, "email": email })),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segment_encodes_unsafe_chars() {
        assert_eq!(encode_path_segment("simple-id_1.2~"), "simple-id_1.2~");
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(encode_path_segment("q?x#y"), "q%3Fx%23y");
        assert_eq!(encode_path_segment("a b&c"), "a%20b%26c");
    }

    #[test]
    fn base_url_trailing_slashes_trimmed() {
        let c = Client::new("http://localhost:7373///", "mc_test");
        assert_eq!(c.base_url, "http://localhost:7373");
    }

    #[test]
    fn latest_management_requests_use_engine_camel_case() {
        let request = DeleteProjectRequest {
            action: "move".into(),
            target_project_id: Some("project_2".into()),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "action": "move",
                "targetProjectId": "project_2"
            })
        );

        let bulk = BulkDeleteDocumentsRequest {
            ids: Some(vec!["doc_1".into()]),
            container_tags: None,
        };
        assert_eq!(
            serde_json::to_value(bulk).unwrap(),
            serde_json::json!({"ids": ["doc_1"], "containerTags": null})
        );
    }

    #[test]
    fn analytics_query_is_encoded() {
        let path = analytics_path(
            "usage",
            &AnalyticsQuery {
                period: Some("7d&unexpected=true".into()),
                page: Some(2),
                limit: Some(50),
            },
        );
        assert_eq!(
            path,
            "/v1/analytics/usage?period=7d%26unexpected%3Dtrue&page=2&limit=50"
        );
    }

    #[test]
    fn router_target_protects_the_outer_request_url() {
        assert_eq!(
            encode_router_target("https://api.example/chat?api-version=1%20beta#fragment"),
            "https://api.example/chat%3Fapi-version=1%2520beta%23fragment"
        );
    }
}
