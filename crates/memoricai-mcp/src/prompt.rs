//! MCP prompt: `context` — inject the user's profile as a priming message.

use memoricai_core::dto::ProfileRequest;
use memoricai_core::model::AuthContext;
use serde_json::{json, Value};

use crate::format::profile_section;
use crate::server::McpState;

pub fn list() -> Value {
    json!({ "prompts": [{
        "name": "context",
        "description": "Inject the user's memory context (stable preferences + recent activity).",
        "arguments": [
            {"name": "containerTag", "description": "Project scope", "required": false}
        ]
    }]})
}

pub async fn get(
    state: &McpState,
    auth: &AuthContext,
    header_tag: Option<&str>,
    params: &Value,
) -> Value {
    let requested = params
        .get("arguments")
        .and_then(|a| a.get("containerTag"))
        .and_then(|v| v.as_str())
        .or(header_tag);
    let tag = match state.auth.scope_tag(auth, requested) {
        Ok(tag) => tag.unwrap_or_else(|| memoricai_core::DEFAULT_CONTAINER_TAG.to_string()),
        Err(error) => {
            return json!({
                "description": "User memory context",
                "messages": [{
                    "role": "user",
                    "content": {"type": "text", "text": error.to_string()}
                }],
                "isError": true
            });
        }
    };

    let preq = ProfileRequest {
        container_tag: tag,
        q: None,
        threshold: None,
        filters: None,
        include: None,
        buckets: None,
    };
    let body = match state.engine.profile(&auth.org.id, &preq).await {
        Ok(p) => format!(
            "You have persistent memory about this user. Save memory-worthy info you learn. \
             ## User Context\n\n{}",
            profile_section(&p.profile)
        ),
        Err(_) => "You have persistent memory about this user.".to_string(),
    };

    json!({
        "description": "User memory context",
        "messages": [{
            "role": "user",
            "content": {"type": "text", "text": body}
        }]
    })
}
