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
        "paths": {
            "/v1/documents": { "post": { "summary": "Ingest content" }, "get": { "summary": "List documents" } },
            "/v1/documents/file": { "post": { "summary": "Upload a file" } },
            "/v1/documents/search": { "post": { "summary": "Document/chunk RAG search" } },
            "/v1/memories": { "post": { "summary": "Create memories" }, "delete": { "summary": "Forget a memory" }, "patch": { "summary": "Versioned update" } },
            "/v1/memories/forget-matching": { "post": { "summary": "Bulk semantic forget" } },
            "/v1/search": { "post": { "summary": "Memory / hybrid search" } },
            "/v1/context": { "post": { "summary": "Build bounded, source-aware LLM context" } },
            "/v1/profile": { "post": { "summary": "Entity profile fast path" } },
            "/v1/projects": { "get": { "summary": "List projects" }, "post": { "summary": "Create project" } },
            "/v1/settings": { "get": { "summary": "Get settings" }, "patch": { "summary": "Update settings" } },
            "/v1/session": { "get": { "summary": "Session / key introspection" } },
            "/v1/auth/scoped-key": { "post": { "summary": "Create container-scoped key" } }
        }
    }))
}
