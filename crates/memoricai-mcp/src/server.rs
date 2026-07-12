//! Router, JSON-RPC dispatch, and bearer auth extraction.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use memoricai_auth::AuthService;
use memoricai_core::model::AuthContext;
use memoricai_engine::Engine;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::{prompt, resources, tools, wellknown, SERVER_VERSION};

#[derive(Clone)]
pub struct McpState {
    pub engine: Engine,
    pub auth: Arc<AuthService>,
}

/// Build the MCP router. Mounted by the binary (typically under a base path).
pub fn mcp_router(engine: Engine, auth: Arc<AuthService>) -> Router {
    let state = McpState { engine, auth };
    Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp))
        .route("/", get(root))
        .route(
            "/.well-known/oauth-protected-resource",
            get(wellknown::protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(wellknown::protected_resource),
        )
        .with_state(state)
}

async fn root() -> Json<Value> {
    Json(json!({ "name": "memoricai-mcp", "version": SERVER_VERSION }))
}

/// GET /mcp: no server-initiated SSE stream in Phase 1 (spec allows 405).
async fn get_mcp() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "SSE stream not supported",
    )
        .into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            "Bearer resource_metadata=\"/.well-known/oauth-protected-resource\"",
        )],
        Json(json!({"error": "unauthorized"})),
    )
        .into_response()
}

fn rpc_ok(id: Value, result: Value) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn rpc_err(id: Value, code: i64, message: impl Into<String>) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}}))
        .into_response()
}

async fn post_mcp(State(state): State<McpState>, headers: HeaderMap, body: Bytes) -> Response {
    let mut req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return rpc_err(Value::Null, -32700, "parse error"),
    };
    let id = req.get("id").cloned();
    let params = req
        .get_mut("params")
        .map(Value::take)
        .unwrap_or_else(|| json!({}));
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // Notifications carry no id and expect no response body.
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };

    let header_tag = headers.get("x-mc-project").and_then(|v| v.to_str().ok());

    let needs_auth = matches!(method, "tools/call" | "resources/read" | "prompts/get");
    let auth = match extract_auth(&headers, &state.auth).await {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    if needs_auth && auth.is_none() {
        return unauthorized();
    }

    match method {
        "initialize" => rpc_ok(id, initialize_result()),
        "tools/list" => rpc_ok(id, tools::list()),
        "tools/call" => {
            let auth = auth.unwrap();
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").unwrap_or(&Value::Null);
            let result = tools::call(&state, &auth, header_tag, name, args).await;
            rpc_ok(id, result)
        }
        "resources/list" => rpc_ok(id, resources::list()),
        "resources/read" => {
            let auth = auth.unwrap();
            let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            match resources::read(&state, &auth, header_tag, uri).await {
                Ok(res) => rpc_ok(id, res),
                Err((code, msg)) => rpc_err(id, code, msg),
            }
        }
        "prompts/list" => rpc_ok(id, prompt::list()),
        "prompts/get" => {
            let auth = auth.unwrap();
            let result = prompt::get(&state, &auth, header_tag, &params).await;
            rpc_ok(id, result)
        }
        other => rpc_err(id, -32601, format!("method not found: {other}")),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "serverInfo": { "name": "memoricai-mcp", "version": SERVER_VERSION },
        "capabilities": { "tools": {}, "resources": {}, "prompts": {} }
    })
}

/// Extract + validate a bearer token.
/// `Ok(None)` = no credentials; `Ok(Some)` = valid; `Err(401)` = present but invalid.
async fn extract_auth(
    headers: &HeaderMap,
    auth: &AuthService,
) -> Result<Option<AuthContext>, Response> {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(None);
    };
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .unwrap_or(value)
        .trim();
    if token.is_empty() {
        return Ok(None);
    }
    // Accept both `mc_` API keys and OAuth2 access tokens.
    match auth.introspect_bearer(token).await {
        Ok(ctx) => Ok(Some(ctx)),
        Err(_) => Err(unauthorized()),
    }
}
