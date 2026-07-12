//! Domain entities. Fields marked `#[serde(skip)]` are internal (tenant scope,
//! hashes) and never leave the API. JSON keys are camelCase for wire-compat.

use crate::enums::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Timestamp = DateTime<Utc>;

/// An ingested piece of source content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_id: Option<String>,
    #[serde(skip)]
    pub content_hash: Option<String>,
    #[serde(skip)]
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub status: DocumentStatus,
    pub metadata: Value,
    pub container_tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_count: Option<i64>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// The atomic knowledge unit: an entity-centric fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(skip)]
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// The fact text (legacy alias `content`).
    pub memory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub mem_type: Option<String>,
    #[serde(skip)]
    pub space_container_tag: String,
    pub version: i32,
    pub is_latest: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_memory_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_memory_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<MemoryRelation>,
    pub source_count: i32,
    pub is_static: bool,
    pub is_inference: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_status: Option<String>,
    pub is_forgotten: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forget_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forget_after: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forget_batch_id: Option<String>,
    /// When the fact's underlying event happened (from extraction), as
    /// opposed to when it was recorded (`created_at`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_date: Option<Timestamp>,
    pub metadata: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A RAG chunk of a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    pub id: String,
    #[serde(skip)]
    pub document_id: String,
    pub content: String,
    #[serde(rename = "type")]
    pub chunk_type: ChunkType,
    pub position: i32,
    pub metadata: Value,
    pub created_at: Timestamp,
}

/// A tenant boundary / project (a.k.a. container tag / space).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Space {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip)]
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    pub container_tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    pub visibility: Visibility,
    pub is_experimental: bool,
    pub metadata: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Materialized per-container profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#static: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buckets: Option<std::collections::BTreeMap<String, Vec<String>>>,
}

/// A topical profile bucket definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileBucket {
    pub key: String,
    pub description: String,
}

/// Per-organization ingestion/settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgSettings {
    pub should_llm_filter: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_items: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_items: Option<Vec<String>>,
    pub chunk_size: i32,
}

impl Default for OrgSettings {
    fn default() -> Self {
        Self {
            should_llm_filter: false,
            filter_prompt: None,
            categories: None,
            include_items: None,
            exclude_items: None,
            chunk_size: -1,
        }
    }
}

// ---------- identity ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub metadata: Value,
}

/// Access scope carried by an authenticated request.
#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    pub id: String,
    /// argon2 hash of the full key
    pub key_hash: String,
    /// first segment for O(1) lookup, e.g. `mc_<orgId>`
    pub prefix: String,
    pub last4: String,
    pub org_id: String,
    pub user_id: Option<String>,
    pub name: String,
    /// "org" (full) or "scoped"
    pub key_type: String,
    pub container_tag: Option<String>,
    pub allowed_endpoints: Option<Vec<String>>,
    pub rate_limit_max: i32,
    pub rate_limit_window_ms: i64,
    pub expires_at: Option<Timestamp>,
    pub revoked: bool,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct Membership {
    pub user_id: String,
    pub org_id: String,
    pub role: OrgRole,
    /// "full" or "restricted"
    pub access_type: String,
    pub container_tags: Vec<String>,
}

// ---------- connectors ----------

/// An external integration (Google Drive, Notion, GitHub, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub provider: String,
    #[serde(skip)]
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub document_limit: i32,
    pub container_tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    pub metadata: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// One execution of a connector sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRun {
    pub id: String,
    pub connection_id: String,
    pub status: String,
    pub trigger_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    pub started_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
    pub items_processed: i32,
    pub items_failed: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Access + refresh credentials for a connection (never leaves the server).
#[derive(Debug, Clone)]
pub struct ConnectionCredentials {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub sync_cursor: Option<String>,
}

/// The resolved identity + scope for a request.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user: User,
    pub org: Organization,
    pub key_id: String,
    pub key_type: String,
    /// Effective OAuth-style permission. API keys are always `write`.
    pub permission: String,
    /// Organization role for user-backed credentials. System keys have no role.
    pub org_role: Option<OrgRole>,
    /// endpoint allowlist for scoped keys (None = full org key)
    pub allowed_endpoints: Option<Vec<String>>,
    /// container tag a scoped key / restricted member is limited to
    pub scoped_container_tag: Option<String>,
    pub restricted_container_tags: Option<Vec<String>>,
}
