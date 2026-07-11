//! memoricai-mcp: a hand-rolled MCP (Model Context Protocol) server over axum
//! Streamable HTTP. No `rmcp` dependency — plain JSON-RPC 2.0 so we control the
//! wire shape exactly and avoid an evolving external API.

mod format;
mod prompt;
mod resources;
mod server;
mod tools;
mod wellknown;

pub use server::mcp_router;

/// MCP server version reported in `initialize` / `GET /` (inventory §6).
pub const SERVER_VERSION: &str = "4.0.0";
/// Max content accepted by the `memory` tool.
pub const MAX_CONTENT: usize = 200_000;
/// Max characters returned by `recall`.
pub const MAX_RECALL_CHARS: usize = 200_000;
/// Similarity threshold for the semantic forget fallback.
pub const FORGET_THRESHOLD: f32 = 0.85;
