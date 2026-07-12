//! memoricai-core: pure domain types, enums, wire DTOs, the metadata-filter AST,
//! provider trait ports, and typed errors. No I/O. Every other crate builds on this.

pub mod dto;
pub mod enums;
pub mod error;
pub mod filter;
pub mod ids;
pub mod model;
pub mod network;
pub mod ports;

pub use error::{Error, Result};

/// Default container tag when none is supplied.
pub const DEFAULT_CONTAINER_TAG: &str = "mc_project_default";

/// Endpoints a container-scoped API key is permitted to call (inventory §2.9).
pub const SCOPED_KEY_ALLOWED_ENDPOINTS: &[&str] = &[
    "/v1/documents",
    "/v1/memories",
    "/v1/search",
    "/v1/profile",
    "/v1/router",
    "/v1/session",
];

/// Validate a container tag against the resolved charset (design.md §2).
/// `^[a-zA-Z0-9_:\-.]+$`, length 1..=100.
pub fn is_valid_container_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 100
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-' | '.'))
}
