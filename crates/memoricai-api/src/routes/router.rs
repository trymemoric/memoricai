//! Memory Router ("infinite chat") — an OpenAI-compatible proxy that injects
//! relevant memories into the chat before forwarding upstream.
//!
//! Clients point at `POST /v1/router/{provider-url}` (e.g.
//! `/v1/router/https://api.openai.com/v1/chat/completions`), forward the provider
//! key as `Authorization`, and pass `x-memoricai-api-key` for memory lookup and
//! optional `x-mc-conversation-id`. On any internal error we transparently
//! passthrough without injection.

use crate::{AppState, RequestLog};
use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use memoricai_core::dto::{MemorySearchRequest, SearchInclude};
use serde_json::Value;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/router/{*target}", post(proxy))
}

pub(crate) async fn proxy(
    State(state): State<AppState>,
    Extension(request_log): Extension<RequestLog>,
    Path(target): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut upstream_body = body.clone();
    let Some(memory_token) = header(&headers, "x-memoricai-api-key") else {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "missing x-memoricai-api-key"})),
        )
            .into_response();
    };
    let ctx = match state.auth.introspect_bearer(&memory_token).await {
        Ok(ctx) => ctx,
        Err(error) => {
            tracing::warn!(%error, "router memory authentication failed");
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({"error": "invalid memory API key"})),
            )
                .into_response();
        }
    };
    request_log.set(&ctx);
    if let Err(error) = state.auth.authorize_endpoint(&ctx, "/v1/router") {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response();
    }
    let requested_tag = header(&headers, "x-mc-project");
    let tag = match state.auth.scope_tag(&ctx, requested_tag.as_deref()) {
        Ok(tag) => tag,
        Err(error) => {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response()
        }
    };
    let (target, client) = match validated_target(&target, &state.router_allowed_origins).await {
        Ok(validated) => validated,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"error": error})),
            )
                .into_response()
        }
    };

    // Best-effort memory injection after authentication and scope resolution.
    if let Ok(mut json) = serde_json::from_slice::<Value>(&body) {
        if let Some(query) = last_user_message(&json) {
            if inject_memories(&state, &ctx.org.id, tag.as_deref(), &query, &mut json)
                .await
                .unwrap_or(false)
            {
                if let Ok(bytes) = serde_json::to_vec(&json) {
                    upstream_body = bytes.into();
                }
            }
        }
    }

    let mut req = client.post(target).body(upstream_body);
    if let Some(auth) = header(&headers, "authorization") {
        req = req.header("authorization", auth);
    }
    req = req.header("content-type", "application/json");

    match req.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let content_type = resp.headers().get("content-type").cloned();
            let mut response = Response::new(Body::from_stream(resp.bytes_stream()));
            *response.status_mut() = status;
            if let Some(content_type) = content_type {
                response
                    .headers_mut()
                    .insert(axum::http::header::CONTENT_TYPE, content_type);
            }
            response
        }
        Err(error) => {
            let request_id = memoricai_core::ids::request_id();
            tracing::warn!(request_id, %error, "router upstream request failed");
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({
                    "error": "upstream_error",
                    "message": format!("upstream request failed (request id: {request_id})")
                })),
            )
                .into_response()
        }
    }
}

async fn validated_target(
    target: &str,
    allowed_origins: &[String],
) -> Result<(reqwest::Url, reqwest::Client), String> {
    let url = reqwest::Url::parse(target).map_err(|_| "invalid upstream URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("upstream URL must be an HTTP(S) URL without credentials or fragments".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "upstream URL is missing a host".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "upstream URL is missing a port".to_string())?;
    let origin = format!("{}://{}:{}", url.scheme(), host, port);
    let explicitly_allowed = allowed_origins.iter().any(|allowed| {
        reqwest::Url::parse(allowed).ok().is_some_and(|allowed| {
            allowed.host_str().is_some_and(|allowed_host| {
                format!(
                    "{}://{}:{}",
                    allowed.scheme(),
                    allowed_host,
                    allowed.port_or_known_default().unwrap_or(0)
                ) == origin
            })
        })
    });
    if !allowed_origins.is_empty() && !explicitly_allowed {
        return Err("upstream origin is not allowlisted".into());
    }
    if allowed_origins.is_empty() && url.scheme() != "https" {
        return Err(
            "public router targets must use HTTPS; configure MEMORICAI_ROUTER_ALLOWED_ORIGINS for local providers"
                .into(),
        );
    }
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "upstream host could not be resolved".to_string())?
        .collect();
    if addresses.is_empty() {
        return Err("upstream host could not be resolved".into());
    }
    if !explicitly_allowed
        && addresses
            .iter()
            .any(|address| memoricai_core::network::is_blocked_ip(address.ip()))
    {
        return Err("upstream resolves to a non-public network address".into());
    }
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none());
    if host.parse::<std::net::IpAddr>().is_err() {
        builder = builder.resolve(host, addresses[0]);
    }
    let client = builder.build().map_err(|error| error.to_string())?;
    Ok((url, client))
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Extract the last user message's text from an OpenAI chat body.
fn last_user_message(json: &Value) -> Option<String> {
    let messages = json.get("messages")?.as_array()?;
    for m in messages.iter().rev() {
        if m.get("role").and_then(|r| r.as_str()) == Some("user") {
            return match m.get("content") {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Array(parts)) => Some(
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                _ => None,
            };
        }
    }
    None
}

/// Prepend a system message with relevant memories. Returns true if injected.
async fn inject_memories(
    state: &AppState,
    org_id: &str,
    tag: Option<&str>,
    query: &str,
    json: &mut Value,
) -> Result<bool, ()> {
    let sreq = MemorySearchRequest {
        q: query.to_string(),
        container_tag: tag.map(|s| s.to_string()),
        search_mode: "hybrid".into(),
        limit: 6,
        threshold: 0.3,
        rerank: false,
        rewrite_query: false,
        filters: None,
        include: SearchInclude::default(),
        digest: false,
    };
    let results = state
        .engine
        .search_memories(org_id, &sreq, tag)
        .await
        .map_err(|_| ())?;
    if results.results.is_empty() {
        return Ok(false);
    }
    let context = results
        .results
        .iter()
        .filter_map(|r| r.memory.as_deref().or(r.chunk.as_deref()))
        .map(|m| format!("- {m}"))
        .collect::<Vec<_>>()
        .join("\n");
    let sys = serde_json::json!({
        "role": "system",
        "content": format!("Relevant memories about the user:\n{context}"),
    });
    if let Some(messages) = json.get_mut("messages").and_then(|m| m.as_array_mut()) {
        messages.insert(0, sys);
        return Ok(true);
    }
    Ok(false)
}
