//! Discovery + health.

use axum::Json;
use serde_json::{json, Value};

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "memoricai",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub async fn openapi() -> Json<Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "memoricai",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "memoricai — Rust memory & context engine (/v1)."
        },
        "servers": [{ "url": "/" }],
        "components": {
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer" }
            }
        },
        "paths": {
            "/": { "get": operation("MCP server metadata") },
            "/health": { "get": operation("Health and engine version") },
            "/v1/openapi": { "get": operation("OpenAPI discovery document") },
            "/v1/documents": {
                "post": operation("Ingest content"),
                "get": operation("List documents")
            },
            "/v1/documents/batch": { "post": operation("Batch ingest content") },
            "/v1/documents/file": { "post": operation("Upload a file") },
            "/v1/documents/list": { "post": operation("List documents with JSON filters") },
            "/v1/documents/documents": { "post": operation("List documents with memories") },
            "/v1/documents/processing": { "get": operation("List documents still processing") },
            "/v1/documents/bulk": { "delete": operation("Bulk-delete documents") },
            "/v1/documents/{id}": {
                "parameters": [path_parameter("id")],
                "get": operation("Get a document"),
                "patch": operation("Update and reprocess a document"),
                "delete": operation("Delete a document")
            },
            "/v1/documents/search": {
                "post": operation("Document/chunk RAG search"),
                "get": operation("Document/chunk RAG search with query parameters")
            },
            "/v1/memories": {
                "post": operation("Create memories"),
                "delete": operation("Forget a memory"),
                "patch": operation("Versioned memory update")
            },
            "/v1/memories/forget-matching": { "post": operation("Bulk semantic forget") },
            "/v1/search": { "post": operation("Memory and hybrid search") },
            "/v1/context": { "post": operation("Build bounded, source-aware LLM context") },
            "/v1/profile": { "post": operation("Entity profile fast path") },
            "/v1/profile/buckets": { "post": operation("List profile buckets") },
            "/v1/buckets": { "post": operation("Create a profile bucket") },
            "/v1/projects": {
                "get": operation("List projects"),
                "post": operation("Create a project")
            },
            "/v1/projects/{id}": {
                "parameters": [path_parameter("id")],
                "delete": operation("Delete or merge a project")
            },
            "/v1/container-tags/list": {
                "get": operation("List container tags"),
                "post": operation("List container tags")
            },
            "/v1/container-tags/{tag}": {
                "parameters": [path_parameter("tag")],
                "patch": operation("Update a container tag"),
                "delete": operation("Delete a container tag")
            },
            "/v1/container-tags/{tag}/inferred": {
                "parameters": [path_parameter("tag")],
                "get": operation("List inferred memories")
            },
            "/v1/container-tags/{tag}/inferred/{memoryId}/review": {
                "parameters": [path_parameter("tag"), path_parameter("memoryId")],
                "post": operation("Review an inferred memory")
            },
            "/v1/settings": {
                "get": operation("Get organization settings"),
                "patch": operation("Update organization settings")
            },
            "/v1/settings/reset": { "post": operation("Reset organization data") },
            "/v1/session": { "get": operation("Session and key introspection") },
            "/v1/auth/scoped-key": { "post": operation("Create a container-scoped key") },
            "/v1/auth/scoped-key/{id}": {
                "parameters": [path_parameter("id")],
                "delete": operation("Revoke a scoped key")
            },
            "/v1/analytics/usage": { "get": operation("Usage analytics") },
            "/v1/analytics/errors": { "get": operation("Error analytics") },
            "/v1/analytics/logs": { "get": operation("Request logs") },
            "/v1/analytics/memory": { "get": operation("Memory analytics") },
            "/v1/analytics/chat": { "get": operation("Chat savings analytics") },
            "/v1/connections": { "get": operation("List data connections") },
            "/v1/connections/list": { "post": operation("Filter data connections") },
            "/v1/connections/{id}": {
                "parameters": [path_parameter("id")],
                "get": operation("Get a data connection"),
                "post": operation("Create a provider connection"),
                "delete": operation("Delete connections")
            },
            "/v1/connections/{id}/import": {
                "parameters": [path_parameter("id")],
                "post": operation("Start a connector sync")
            },
            "/v1/connections/{id}/sync-runs": {
                "parameters": [path_parameter("id")],
                "get": operation("List connector sync runs")
            },
            "/v1/connections/{id}/resources": {
                "parameters": [path_parameter("id")],
                "get": operation("List connector resources")
            },
            "/v1/connections/{id}/configure": {
                "parameters": [path_parameter("id")],
                "post": operation("Configure a connection")
            },
            "/v1/connections/auth/callback/{provider}": {
                "parameters": [path_parameter("provider")],
                "get": operation("Connector OAuth callback")
            },
            "/v1/connections/webhooks/{provider}": {
                "parameters": [path_parameter("provider")],
                "post": operation("Connector webhook")
            },
            "/v1/router/{target}": {
                "parameters": [path_parameter("target")],
                "post": operation("Memory-injecting LLM proxy")
            },
            "/v1/mcp/session-with-key": { "get": operation("Exchange an MCP token for session details") },
            "/v1/mcp/connect-scope": { "post": operation("Configure MCP project scope") },
            "/v1/admin/provision": { "post": operation("Provision an organization") },
            "/api/auth/oauth2/authorize": { "get": operation("OAuth authorization") },
            "/api/auth/oauth2/consent": { "post": operation("OAuth consent") },
            "/api/auth/oauth2/token": { "post": operation("OAuth token exchange") },
            "/api/auth/oauth2/register": { "post": operation("OAuth dynamic client registration") },
            "/.well-known/oauth-authorization-server": { "get": operation("OAuth authorization server metadata") },
            "/.well-known/openid-configuration": { "get": operation("OpenID configuration") },
            "/.well-known/oauth-protected-resource": { "get": operation("MCP protected-resource metadata") },
            "/.well-known/oauth-protected-resource/mcp": { "get": operation("MCP protected-resource metadata") },
            "/mcp": { "post": operation("MCP Streamable HTTP transport") }
        }
    }))
}

fn operation(summary: &str) -> Value {
    json!({
        "summary": summary,
        "responses": { "200": { "description": "Success" } }
    })
}

fn path_parameter(name: &str) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "schema": { "type": "string" }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discovery_covers_every_feature_family() {
        let Json(document) = openapi().await;
        let paths = document["paths"].as_object().expect("paths object");
        for path in [
            "/v1/documents/batch",
            "/v1/documents/{id}",
            "/v1/context",
            "/v1/profile/buckets",
            "/v1/container-tags/{tag}/inferred/{memoryId}/review",
            "/v1/analytics/usage",
            "/v1/connections/{id}/resources",
            "/v1/router/{target}",
            "/api/auth/oauth2/token",
            "/mcp",
        ] {
            assert!(paths.contains_key(path), "missing discovery path {path}");
        }

        for (path, item) in paths {
            let declared = item["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|parameter| parameter["name"].as_str())
                .collect::<Vec<_>>();
            let mut remainder = path.as_str();
            while let Some(start) = remainder.find('{') {
                let after_start = &remainder[start + 1..];
                let end = after_start.find('}').expect("closed path parameter");
                let name = &after_start[..end];
                assert!(
                    declared.contains(&name),
                    "{path} does not declare path parameter {name}"
                );
                remainder = &after_start[end + 1..];
            }
        }
    }
}
