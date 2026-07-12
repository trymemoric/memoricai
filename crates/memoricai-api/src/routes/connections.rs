//! `/v1/connections/*` — connector CRUD, OAuth callback, sync, and webhooks.

use crate::{ApiError, ApiResult, AppState, Auth};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use memoricai_connectors::Connectors;
use memoricai_core::dto::{
    ConnectionListRequest, CreateConnectionRequest, CreateConnectionResponse, ImportRequest,
};
use memoricai_core::error::Error;
use memoricai_core::model::{Connection, SyncRun};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/connections", get(list_get))
        .route("/v1/connections/list", post(list_post))
        .route(
            "/v1/connections/{id}",
            post(create).get(get_one).delete(delete_one),
        )
        .route("/v1/connections/{id}/import", post(import))
        .route("/v1/connections/{id}/sync-runs", get(sync_runs))
        .route("/v1/connections/{id}/resources", get(resources))
        .route("/v1/connections/{id}/configure", post(configure))
        .route(
            "/v1/connections/auth/callback/{provider}",
            get(oauth_callback),
        )
        .route(
            "/v1/connections/webhooks/{provider}",
            post(webhook).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
}

fn connectors(state: &AppState) -> Connectors {
    Connectors::new(state.engine.clone())
}

fn redact_connection(mut connection: Connection) -> Connection {
    fn redact(value: &mut Value) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
                    if normalized.contains("secret")
                        || normalized.contains("password")
                        || normalized.contains("token")
                        || normalized == "apikey"
                    {
                        *value = Value::String("[REDACTED]".into());
                    } else {
                        redact(value);
                    }
                }
            }
            Value::Array(values) => values.iter_mut().for_each(redact),
            _ => {}
        }
    }
    redact(&mut connection.metadata);
    connection
}

fn base_url(headers: &HeaderMap) -> ApiResult<String> {
    let configured = std::env::var("MEMORICAI_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let candidate = configured.clone().unwrap_or_else(|| {
        let host = headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost:7373");
        format!("http://{host}")
    });
    let url = url::Url::parse(&candidate)
        .map_err(|_| ApiError(Error::BadRequest("invalid MEMORICAI_BASE_URL".into())))?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1")
    );
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
    {
        return Err(ApiError(Error::BadRequest(
            "MEMORICAI_BASE_URL must be HTTPS (or loopback HTTP) without credentials, query, or fragment"
                .into(),
        )));
    }
    if configured.is_none() && !loopback {
        return Err(ApiError(Error::BadRequest(
            "MEMORICAI_BASE_URL is required for non-loopback connector OAuth".into(),
        )));
    }
    Ok(candidate.trim_end_matches('/').to_string())
}

pub async fn create(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CreateConnectionRequest>,
) -> ApiResult<Json<CreateConnectionResponse>> {
    state.auth.authorize_admin(&ctx)?;
    if !Connectors::supported().contains(&provider.as_str()) {
        return Err(ApiError(Error::BadRequest(format!(
            "unknown provider: {provider}"
        ))));
    }
    let base = base_url(&headers)?;
    let resp = connectors(&state)
        .create(&ctx.org.id, Some(&ctx.user.id), &provider, &req, &base)
        .await?;
    Ok(Json(resp))
}

pub async fn list_post(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<ConnectionListRequest>,
) -> ApiResult<Json<Vec<Connection>>> {
    state.auth.authorize_admin(&ctx)?;
    let conns = connectors(&state)
        .list(
            &ctx.org.id,
            req.provider.as_deref(),
            req.container_tags.as_deref(),
        )
        .await?;
    Ok(Json(conns.into_iter().map(redact_connection).collect()))
}

pub async fn list_get(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ApiResult<Json<Vec<Connection>>> {
    state.auth.authorize_admin(&ctx)?;
    let conns = connectors(&state).list(&ctx.org.id, None, None).await?;
    Ok(Json(conns.into_iter().map(redact_connection).collect()))
}

pub async fn get_one(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Connection>> {
    state.auth.authorize_admin(&ctx)?;
    let conn = connectors(&state)
        .get(&ctx.org.id, &id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("connection {id}"))))?;
    Ok(Json(redact_connection(conn)))
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default = "default_true")]
    delete_documents: bool,
}
fn default_true() -> bool {
    true
}

pub async fn delete_one(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    // If `id` is a known connection, delete it; otherwise treat as a provider name.
    let c = connectors(&state);
    if let Some(conn) = c.get(&ctx.org.id, &id).await? {
        c.delete(&ctx.org.id, &id, q.delete_documents).await?;
        return Ok(Json(json!({ "id": conn.id, "provider": conn.provider })));
    }
    // provider-scoped delete: remove all connections of that provider.
    let conns = c.list(&ctx.org.id, Some(&id), None).await?;
    for conn in &conns {
        c.delete(&ctx.org.id, &conn.id, q.delete_documents).await?;
    }
    Ok(Json(json!({ "provider": id, "deleted": conns.len() })))
}

pub async fn import(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
    Json(_req): Json<ImportRequest>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    // `id` may be a connection id or a provider name; import matching connections.
    let c = connectors(&state);
    if c.get(&ctx.org.id, &id).await?.is_some() {
        c.import(&ctx.org.id, &id, "manual").await?;
        return Ok(Json(
            json!({ "message": "sync started", "connectionId": id }),
        ));
    }
    let conns = c.list(&ctx.org.id, Some(&id), None).await?;
    for conn in &conns {
        c.import(&ctx.org.id, &conn.id, "manual").await?;
    }
    Ok(Json(json!({ "message": "sync started", "provider": id })))
}

pub async fn sync_runs(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<SyncRun>>> {
    state.auth.authorize_admin(&ctx)?;
    let runs = connectors(&state).sync_runs(&ctx.org.id, &id).await?;
    Ok(Json(runs))
}

#[derive(Debug, Deserialize)]
pub struct ResourceQuery {
    #[serde(default = "one")]
    page: u32,
    #[serde(default = "thirty")]
    per_page: u32,
}
fn one() -> u32 {
    1
}
fn thirty() -> u32 {
    30
}

pub async fn resources(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
    Query(q): Query<ResourceQuery>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    if q.page == 0 || q.per_page == 0 || q.per_page > 100 {
        return Err(ApiError(Error::BadRequest(
            "page must be positive and perPage must be between 1 and 100".into(),
        )));
    }
    let res = connectors(&state)
        .resources(&ctx.org.id, &id, q.page, q.per_page)
        .await?;
    Ok(Json(res))
}

pub async fn configure(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    let res = connectors(&state).configure(&ctx.org.id, &id, body).await?;
    Ok(Json(res))
}

pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let code = params
        .get("code")
        .ok_or_else(|| ApiError(Error::BadRequest("missing code".into())))?;
    let csrf = params.get("state").map(|s| s.as_str()).unwrap_or("");
    if provider.len() > 64 || code.len() > 4096 || csrf.len() > 512 {
        return Err(ApiError(Error::BadRequest(
            "OAuth callback parameter exceeds its size limit".into(),
        )));
    }
    let redirect = connectors(&state)
        .oauth_callback(&provider, code, csrf)
        .await?;
    Ok(Redirect::to(&redirect).into_response())
}

pub async fn webhook(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let hdrs: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|val| (k.as_str().to_lowercase(), val.to_string()))
        })
        .collect();
    match connectors(&state)
        .handle_webhook(&provider, &hdrs, &body)
        .await
    {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(error) => {
            let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_REQUEST);
            if status.is_server_error() {
                let request_id = memoricai_core::ids::request_id();
                tracing::error!(request_id, %error, "connector webhook failed");
                (status, format!("internal error (request id: {request_id})")).into_response()
            } else {
                (status, error.to_string()).into_response()
            }
        }
    }
}
