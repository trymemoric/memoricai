//! RFC 9728 OAuth protected-resource discovery metadata.

use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

/// Absolute base URL for discovery documents: the configured `MEMORICAI_BASE_URL`,
/// else the request `Host` (loopback dev fallback). Mirrors the API's base_url logic.
fn base_url(headers: &HeaderMap) -> String {
    if let Ok(configured) = std::env::var("MEMORICAI_BASE_URL") {
        let trimmed = configured.trim();
        if !trimmed.is_empty() {
            return trimmed.trim_end_matches('/').to_string();
        }
    }
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost:7373");
    format!("http://{host}")
}

pub async fn protected_resource(headers: HeaderMap) -> Json<Value> {
    // RFC 9728 requires an absolute resource identifier and at least one authorization
    // server so MCP clients can discover where to obtain a token. A relative `resource`
    // and an empty `authorization_servers` (the previous values) break that discovery.
    // Only `offline_access` is advertised: the server issues opaque OAuth2 tokens, not
    // OIDC ID tokens, so it must not claim the `openid` scope.
    let base = base_url(&headers);
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "scopes_supported": ["offline_access"],
        "bearer_methods_supported": ["header"]
    }))
}
