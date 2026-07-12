//! RFC 9728 OAuth protected-resource discovery metadata.

use axum::Json;
use serde_json::{json, Value};

pub async fn protected_resource() -> Json<Value> {
    Json(json!({
        "resource": "/mcp",
        "authorization_servers": [],
        "scopes_supported": ["openid", "profile", "email", "offline_access"],
        "bearer_methods_supported": ["header"]
    }))
}
