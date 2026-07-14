//! Wire DTOs for the `/v1` HTTP surface. camelCase JSON, defaults chosen
//! per `docs/design.md` §2. Kept intentionally permissive on input (optional +
//! defaulted) to tolerate SDK variance.

use crate::model::{Document, Profile};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_limit() -> u32 {
    10
}
fn default_threshold() -> f32 {
    0.5
}
fn default_search_mode() -> String {
    "hybrid".to_string()
}
fn default_context_mode() -> String {
    "auto".to_string()
}
fn default_context_budget_tokens() -> u32 {
    12_000
}
fn default_context_max_sources() -> u32 {
    8
}
fn default_true() -> bool {
    true
}

// ---------------- ingestion ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestRequest {
    pub content: String,
    #[serde(default)]
    pub custom_id: Option<String>,
    #[serde(default)]
    pub container_tag: Option<String>,
    #[serde(default)]
    pub container_tags: Option<Vec<String>>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub entity_context: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub raw: Option<String>,
}

impl IngestRequest {
    /// Resolve the effective single container tag (singular wins, else first of plural).
    pub fn resolved_container_tags(&self) -> Vec<String> {
        if let Some(t) = &self.container_tag {
            vec![t.clone()]
        } else if let Some(ts) = &self.container_tags {
            ts.clone()
        } else {
            vec![]
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestResponse {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchIngestRequest {
    pub documents: Vec<IngestRequest>,
    #[serde(default)]
    pub container_tag: Option<String>,
    #[serde(default)]
    pub entity_context: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchIngestItem {
    pub id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchIngestResponse {
    pub results: Vec<BatchIngestItem>,
    pub success: usize,
    pub failed: usize,
}

// ---------------- listing ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pagination {
    pub current_page: u32,
    pub limit: u32,
    pub total_items: u64,
    pub total_pages: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentListRequest {
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub container_tags: Option<Vec<String>>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentListResponse {
    pub memories: Vec<Document>,
    pub pagination: Pagination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkDeleteResponse {
    pub success: bool,
    pub deleted_count: u64,
}

// ---------------- document search ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSearchRequest {
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub container_tags: Option<Vec<String>>,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default)]
    pub rerank: bool,
    #[serde(default)]
    pub rewrite_query: bool,
    #[serde(default = "default_threshold")]
    pub chunk_threshold: f32,
    #[serde(default = "default_threshold")]
    pub document_threshold: f32,
    #[serde(default)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub include_full_docs: bool,
    #[serde(default)]
    pub include_summary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkHit {
    pub content: String,
    pub score: f32,
    pub is_relevant: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSearchResult {
    pub document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub score: f32,
    pub chunks: Vec<ChunkHit>,
    pub metadata: Value,
    pub created_at: crate::model::Timestamp,
    pub updated_at: crate::model::Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSearchResponse {
    pub results: Vec<DocumentSearchResult>,
    pub timing: u64,
    pub total: usize,
}

// ---------------- memory search ----------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchInclude {
    #[serde(default)]
    pub documents: bool,
    #[serde(default)]
    pub related_memories: bool,
    #[serde(default)]
    pub forgotten_memories: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchRequest {
    pub q: String,
    #[serde(default)]
    pub container_tag: Option<String>,
    #[serde(default = "default_search_mode")]
    pub search_mode: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default)]
    pub rerank: bool,
    #[serde(default)]
    pub rewrite_query: bool,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default)]
    pub include: SearchInclude,
    /// Compose a compact, date-stamped digest of the top matching memories
    /// (grouped by source document, latest versions only) alongside the
    /// results. Intended as ready-to-inject LLM context.
    #[serde(default)]
    pub digest: bool,
}

impl Default for MemorySearchRequest {
    fn default() -> Self {
        Self {
            q: String::new(),
            container_tag: None,
            search_mode: default_search_mode(),
            limit: default_limit(),
            threshold: default_threshold(),
            rerank: false,
            rewrite_query: false,
            filters: None,
            include: SearchInclude::default(),
            digest: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub memory: String,
    pub relation: String,
    pub version: i32,
    pub updated_at: crate::model::Timestamp,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryContext {
    pub parents: Vec<ContextEntry>,
    pub children: Vec<ContextEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchResult {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<String>,
    pub similarity: f32,
    pub metadata: Value,
    pub updated_at: crate::model::Timestamp,
    pub version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_memory_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<MemoryContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<Document>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchResponse {
    pub results: Vec<MemorySearchResult>,
    pub timing: u64,
    pub total: usize,
    /// Present when the request set `digest: true` and memories matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

// ---------------- ready-to-inject context ----------------

/// Build bounded, source-diverse context from memory and document retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextRequest {
    pub q: String,
    #[serde(default)]
    pub container_tag: Option<String>,
    /// `auto`, `lookup`, or `aggregation`. `auto` uses the deterministic
    /// aggregation-query detector.
    #[serde(default = "default_context_mode")]
    pub mode: String,
    /// Approximate input budget. The engine uses a documented four-characters-per-token
    /// estimate and reports the resulting estimate in diagnostics.
    #[serde(default = "default_context_budget_tokens")]
    pub budget_tokens: u32,
    /// Maximum distinct source documents represented in the final context.
    #[serde(default = "default_context_max_sources")]
    pub max_sources: u32,
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default)]
    pub rewrite_query: bool,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default = "default_true")]
    pub include_digest: bool,
}

impl Default for ContextRequest {
    fn default() -> Self {
        Self {
            q: String::new(),
            container_tag: None,
            mode: default_context_mode(),
            budget_tokens: default_context_budget_tokens(),
            max_sources: default_context_max_sources(),
            threshold: default_threshold(),
            rewrite_query: false,
            filters: None,
            include_digest: true,
        }
    }
}

/// One source considered by the context packer. Omitted sources remain in the response
/// with `included=false` and an explicit `omissionReason`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEvidence {
    pub rank: u32,
    pub source_id: String,
    pub document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub score: f32,
    pub included: bool,
    pub available_chars: usize,
    pub included_chars: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omission_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextOmission {
    pub rank: u32,
    pub source_id: String,
    pub document_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDiagnostics {
    pub mode: String,
    pub aggregation_query: bool,
    pub budget_tokens: u32,
    pub budget_chars: usize,
    pub used_chars: usize,
    pub estimated_tokens: usize,
    pub digest_chars: usize,
    pub evidence_chars: usize,
    pub sources_considered: usize,
    pub sources_selected: usize,
    pub sources_included: usize,
    pub sources_omitted: usize,
    pub truncated_sources: usize,
    pub digest_truncated: bool,
    /// Always false for the bounded packer: truncation happens within individual
    /// evidence blocks, never by slicing the assembled context.
    pub hard_truncated: bool,
    pub omissions: Vec<ContextOmission>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextResponse {
    pub context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub evidence: Vec<ContextEvidence>,
    pub diagnostics: ContextDiagnostics,
    pub timing: u64,
}

// ---------------- memories ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInput {
    pub content: String,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMemoriesRequest {
    pub memories: Vec<MemoryInput>,
    pub container_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedMemory {
    pub id: String,
    pub memory: String,
    pub is_static: bool,
    pub created_at: crate::model::Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMemoriesResponse {
    pub document_id: Option<String>,
    pub memories: Vec<CreatedMemory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    pub container_tag: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchMemoryRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    pub new_content: String,
    #[serde(default)]
    pub metadata: Option<Value>,
}

fn default_max_forget() -> u32 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetMatchingRequest {
    pub query: String,
    pub container_tag: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default = "default_max_forget")]
    pub max_forget: u32,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetCandidate {
    pub id: String,
    pub memory: String,
    pub similarity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetMatchingResponse {
    pub dry_run: bool,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forget_batch_id: Option<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<ForgetCandidate>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forgotten: Option<Vec<ForgetCandidate>>,
}

// ---------------- profile ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRequest {
    pub container_tag: String,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub threshold: Option<f32>,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub buckets: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileResponse {
    pub profile: Profile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_results: Option<MemorySearchResponse>,
}

// ---------------- projects / container tags ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub container_tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    pub created_at: crate::model::Timestamp,
    pub updated_at: crate::model::Timestamp,
    pub is_experimental: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub emoji: Option<String>,
}

// ---------------- settings ----------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub should_llm_filter: Option<bool>,
    #[serde(default)]
    pub filter_prompt: Option<String>,
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    #[serde(default)]
    pub include_items: Option<Vec<String>>,
    #[serde(default)]
    pub exclude_items: Option<Vec<String>>,
    #[serde(default)]
    pub chunk_size: Option<i32>,
}

// ---------------- auth / session ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub user: crate::model::User,
    pub org: crate::model::Organization,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateScopedKeyRequest {
    pub container_tag: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub expires_in_days: Option<i64>,
    #[serde(default)]
    pub rate_limit_max: Option<i32>,
    #[serde(default)]
    pub rate_limit_time_window: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateScopedKeyResponse {
    pub key: String,
    pub id: String,
    pub name: String,
    pub container_tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<crate::model::Timestamp>,
    pub allowed_endpoints: Vec<String>,
}

// ---------------- analytics ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    #[serde(rename = "type")]
    pub kind: String,
    pub count: i64,
    pub avg_duration: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsUsageResponse {
    pub usage: Vec<UsageEntry>,
    pub total_memories: i64,
    pub pagination: Pagination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCount {
    pub status_code: i32,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsErrorsResponse {
    pub total_errors: i64,
    pub error_rate: f64,
    pub by_status_code: Vec<StatusCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: String,
    pub created_at: crate::model::Timestamp,
    #[serde(rename = "type")]
    pub kind: String,
    pub status_code: Option<i32>,
    pub duration: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsLogsResponse {
    pub logs: Vec<LogEntry>,
    pub pagination: Pagination,
}

// ---------------- profile buckets ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BucketsResponse {
    pub buckets: Vec<crate::model::ProfileBucket>,
}

// ---------------- inferred-memory review ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferredMemoryDto {
    pub id: String,
    pub memory: String,
    pub parent_count: i32,
    pub created_at: crate::model::Timestamp,
    pub updated_at: crate::model::Timestamp,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferredListResponse {
    pub memories: Vec<InferredMemoryDto>,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequest {
    pub action: String, // approve | decline | undo
}

// ---------------- connections ----------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConnectionRequest {
    #[serde(default)]
    pub redirect_url: Option<String>,
    #[serde(default)]
    pub container_tags: Option<Vec<String>>,
    #[serde(default)]
    pub document_limit: Option<i32>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConnectionResponse {
    pub id: String,
    pub auth_link: Option<String>,
    pub expires_in: Option<String>,
    pub redirects_to: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionListRequest {
    #[serde(default)]
    pub container_tags: Option<Vec<String>>,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {}

// ---------------- oauth2 ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterClientRequest {
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub grant_types: Option<Vec<String>>,
    #[serde(default, rename = "token_endpoint_auth_method")]
    pub token_endpoint_auth_method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterClientResponse {
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub token_endpoint_auth_method: String,
}

/// MCP token→key exchange response (`GET /v1/mcp/session-with-key`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWithKeyResponse {
    pub user_id: String,
    pub api_key: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
