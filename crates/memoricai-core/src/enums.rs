//! Strict enums with wire-stable string values. `document_type` and
//! `request_type` are intentionally kept as free `String`s elsewhere (docs say
//! "keep text fallback"), so they are not modeled here.

use serde::{Deserialize, Serialize};

/// Pipeline status of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentStatus {
    Unknown,
    Queued,
    Extracting,
    Chunking,
    Embedding,
    Indexing,
    Done,
    Failed,
}

impl DocumentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DocumentStatus::Unknown => "unknown",
            DocumentStatus::Queued => "queued",
            DocumentStatus::Extracting => "extracting",
            DocumentStatus::Chunking => "chunking",
            DocumentStatus::Embedding => "embedding",
            DocumentStatus::Indexing => "indexing",
            DocumentStatus::Done => "done",
            DocumentStatus::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "queued" => DocumentStatus::Queued,
            "extracting" => DocumentStatus::Extracting,
            "chunking" => DocumentStatus::Chunking,
            "embedding" => DocumentStatus::Embedding,
            "indexing" => DocumentStatus::Indexing,
            "done" => DocumentStatus::Done,
            "failed" => DocumentStatus::Failed,
            _ => DocumentStatus::Unknown,
        }
    }
}

/// Relationship between two memories in the temporal knowledge graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryRelation {
    /// new supersedes/replaces old (version-chain supersession)
    Updates,
    /// builds on / adds detail; both stay valid
    Extends,
    /// inferred from patterns; also the doc→memory containment edge
    Derives,
}

impl MemoryRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryRelation::Updates => "updates",
            MemoryRelation::Extends => "extends",
            MemoryRelation::Derives => "derives",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "extends" => MemoryRelation::Extends,
            "derives" => MemoryRelation::Derives,
            // unknown types coerce to updates (per inventory graph rules)
            _ => MemoryRelation::Updates,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkType {
    Text,
    Image,
}

impl ChunkType {
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkType::Text => "text",
            ChunkType::Image => "image",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "image" => ChunkType::Image,
            _ => ChunkType::Text,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
    Unlisted,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
            Visibility::Unlisted => "unlisted",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "public" => Visibility::Public,
            "unlisted" => Visibility::Unlisted,
            _ => Visibility::Private,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceRole {
    Owner,
    Admin,
    Editor,
    Viewer,
}

impl SpaceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceRole::Owner => "owner",
            SpaceRole::Admin => "admin",
            SpaceRole::Editor => "editor",
            SpaceRole::Viewer => "viewer",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "owner" => SpaceRole::Owner,
            "admin" => SpaceRole::Admin,
            "editor" => SpaceRole::Editor,
            _ => SpaceRole::Viewer,
        }
    }
}

/// Organization membership role (owner > admin > member).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrgRole {
    Owner,
    Admin,
    Member,
}

impl OrgRole {
    pub fn as_str(self) -> &'static str {
        match self {
            OrgRole::Owner => "owner",
            OrgRole::Admin => "admin",
            OrgRole::Member => "member",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "owner" => OrgRole::Owner,
            "admin" => OrgRole::Admin,
            _ => OrgRole::Member,
        }
    }
    /// Numeric rank for hierarchy comparisons (higher = more powerful).
    pub fn rank(self) -> u8 {
        match self {
            OrgRole::Owner => 3,
            OrgRole::Admin => 2,
            OrgRole::Member => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionProvider {
    Notion,
    GoogleDrive,
    Onedrive,
    Gmail,
    Github,
    WebCrawler,
    S3,
    Granola,
}

impl ConnectionProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionProvider::Notion => "notion",
            ConnectionProvider::GoogleDrive => "google-drive",
            ConnectionProvider::Onedrive => "onedrive",
            ConnectionProvider::Gmail => "gmail",
            ConnectionProvider::Github => "github",
            ConnectionProvider::WebCrawler => "web-crawler",
            ConnectionProvider::S3 => "s3",
            ConnectionProvider::Granola => "granola",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "notion" => ConnectionProvider::Notion,
            "google-drive" => ConnectionProvider::GoogleDrive,
            "onedrive" => ConnectionProvider::Onedrive,
            "gmail" => ConnectionProvider::Gmail,
            "github" => ConnectionProvider::Github,
            "web-crawler" => ConnectionProvider::WebCrawler,
            "s3" => ConnectionProvider::S3,
            "granola" => ConnectionProvider::Granola,
            _ => return None,
        })
    }
}

/// Common document type constants (kept as `&str`; storage uses free text).
pub mod doc_type {
    pub const TEXT: &str = "text";
    pub const MARKDOWN: &str = "md";
    pub const WEBPAGE: &str = "webpage";
    pub const PDF: &str = "pdf";
    pub const CODE: &str = "code";
    pub const IMAGE: &str = "image";
    pub const VIDEO: &str = "video";
    pub const AUDIO: &str = "audio";
    pub const JSON: &str = "json";
    pub const CSV: &str = "csv";
    pub const TWEET: &str = "tweet";
    pub const YOUTUBE: &str = "youtube";
}
