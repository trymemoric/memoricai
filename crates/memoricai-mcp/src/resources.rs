//! MCP resources: profile (text), projects (json), and the graph UI app (html stub).

use memoricai_core::dto::ProfileRequest;
use memoricai_core::model::AuthContext;
use serde_json::{json, Value};

use crate::format::profile_section;
use crate::server::McpState;

pub fn list() -> Value {
    json!({ "resources": [
        {"uri": "memoricai://profile", "name": "User Profile", "mimeType": "text/plain"},
        {"uri": "memoricai://projects", "name": "Projects", "mimeType": "application/json"},
        {"uri": "ui://memory-graph/mcp-app.html", "name": "Memory Graph", "mimeType": "text/html"}
    ]})
}

fn tag_of(
    state: &McpState,
    auth: &AuthContext,
    header_tag: Option<&str>,
) -> Result<String, (i64, String)> {
    state
        .auth
        .scope_tag(auth, header_tag)
        .map(|tag| tag.unwrap_or_else(|| memoricai_core::DEFAULT_CONTAINER_TAG.to_string()))
        .map_err(|error| (-32602, error.to_string()))
}

pub async fn read(
    state: &McpState,
    auth: &AuthContext,
    header_tag: Option<&str>,
    uri: &str,
) -> Result<Value, (i64, String)> {
    match uri {
        "memoricai://profile" => {
            let tag = tag_of(state, auth, header_tag)?;
            let preq = ProfileRequest {
                container_tag: tag,
                q: None,
                threshold: None,
                filters: None,
                include: None,
                buckets: None,
            };
            let text = match state.engine.profile(&auth.org.id, &preq).await {
                Ok(p) => format!("# User Profile\n\n{}", profile_section(&p.profile)),
                Err(_) => "# User Profile\n\n_(none)_".to_string(),
            };
            Ok(contents_text(uri, "text/plain", text))
        }
        "memoricai://projects" => {
            let allowed = state.auth.allowed_container_tags(auth);
            let tags: Vec<String> = state
                .engine
                .db
                .list_spaces(&auth.org.id)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|space| {
                    allowed
                        .as_ref()
                        .is_none_or(|tags| tags.iter().any(|tag| tag == &space.container_tag))
                })
                .map(|s| s.container_tag)
                .collect();
            let body = json!({ "projects": tags }).to_string();
            Ok(contents_text(uri, "application/json", body))
        }
        "ui://memory-graph/mcp-app.html" => {
            Ok(contents_text(uri, "text/html", GRAPH_APP_HTML.to_string()))
        }
        other => Err((-32602, format!("unknown resource: {other}"))),
    }
}

fn contents_text(uri: &str, mime: &str, text: String) -> Value {
    json!({ "contents": [{"uri": uri, "mimeType": mime, "text": text}] })
}

/// Phase 1 placeholder for the interactive graph app (the full force-graph HTML
/// is deferred; this keeps the resource wire-shape correct).
const GRAPH_APP_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Memory Graph</title></head><body><main id="app"><p>memoricai memory graph (Phase 1 placeholder). Use the <code>fetch-graph-data</code> tool for data.</p></main></body></html>"#;
