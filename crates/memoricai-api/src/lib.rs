//! memoricai-api: the axum HTTP surface for `/v1`, with API-key auth,
//! tenant scoping, error shapes, and OpenAPI. `build_router` returns the app the
//! binary mounts.

pub mod routes;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::extract::FromRequestParts;
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, PRAGMA, REFERRER_POLICY,
    WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{request::Parts, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use memoricai_auth::AuthService;
use memoricai_core::model::AuthContext;
use memoricai_engine::Engine;

#[derive(Clone)]
pub struct AppState {
    pub engine: Engine,
    pub auth: Arc<AuthService>,
    pub request_body_timeout: std::time::Duration,
    /// Exact upstream origins permitted for the Memory Router. Empty = public HTTPS only.
    pub router_allowed_origins: Arc<Vec<String>>,
    /// Master credential for `POST /v1/admin/provision`. `None` = endpoint disabled (404).
    pub provision_key: Option<Arc<str>>,
}

/// Error wrapper mapping [`memoricai_core::Error`] to an HTTP JSON error response.
pub struct ApiError(pub memoricai_core::Error);

impl From<memoricai_core::Error> for ApiError {
    fn from(e: memoricai_core::Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // Never leak internal DB / upstream-provider detail to clients: for 5xx faults,
        // log the real error server-side under a request id and return a generic message.
        let message = if status.is_server_error() {
            let request_id = memoricai_core::ids::request_id();
            tracing::error!(request_id, error = %self.0, "request failed");
            format!("internal error (request id: {request_id})")
        } else {
            self.0.to_string()
        };
        let body = Json(serde_json::json!({
            "error": self.0.code(),
            "message": message,
        }));
        let mut resp = (status, body).into_response();
        if status == StatusCode::UNAUTHORIZED {
            resp.headers_mut().insert(
                WWW_AUTHENTICATE,
                HeaderValue::from_static(
                    "Bearer resource_metadata=\"/.well-known/oauth-protected-resource\"",
                ),
            );
        }
        resp
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

/// Authenticated request context extractor. Runs API-key introspection and the
/// scoped-key endpoint allowlist on every guarded route.
pub struct Auth(pub AuthContext);

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim)
            .ok_or_else(|| {
                ApiError(memoricai_core::Error::Unauthorized(
                    "missing bearer token".into(),
                ))
            })?;
        let ctx = state.auth.introspect_bearer(token).await?;
        // Resource scope is checked by handlers after path/body extraction.
        state
            .auth
            .authorize_request(&ctx, parts.method.as_str(), parts.uri.path())?;
        if let Some(slot) = parts.extensions.get::<RequestLog>() {
            slot.set(&ctx);
        }
        Ok(Auth(ctx))
    }
}

/// Resolved identity captured for one request: `(org_id, user_id, key_id)`.
type RequestIdentity = (String, Option<String>, Option<String>);

/// Per-request slot the [`Auth`] extractor fills with the resolved identity so the
/// access-log middleware can attribute the request in `api_requests`.
#[derive(Clone, Default)]
pub(crate) struct RequestLog(std::sync::Arc<std::sync::Mutex<Option<RequestIdentity>>>);

impl RequestLog {
    pub(crate) fn set(&self, ctx: &AuthContext) {
        *self.0.lock().unwrap() = Some((
            ctx.org.id.clone(),
            Some(ctx.user.id.clone()),
            Some(ctx.key_id.clone()),
        ));
    }
}

/// Records every request (coarse path category, status, duration, and — when
/// authenticated — org/user/key) so analytics is not limited to Memory Router calls.
async fn access_log(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let start = std::time::Instant::now();
    let path = req.uri().path().to_string();
    let slot = RequestLog::default();
    req.extensions_mut().insert(slot.clone());
    let resp = next.run(req).await;
    let status = resp.status().as_u16() as i32;
    let duration = start.elapsed().as_millis() as i64;
    let identity = slot.0.lock().unwrap().take();
    let req_type = request_type(&path);
    let (org, user, key) = identity
        .map(|(org, user, key)| (Some(org), user, key))
        .unwrap_or((None, None, None));
    let _ = state
        .engine
        .db
        .log_request(
            &req_type,
            org.as_deref(),
            user.as_deref(),
            key.as_deref(),
            status,
            duration,
        )
        .await;
    resp
}

fn apply_security_headers(headers: &mut HeaderMap) {
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; \
             frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
}

pub async fn security_headers(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(req).await;
    apply_security_headers(response.headers_mut());
    response
}

fn request_type(path: &str) -> String {
    let segments: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match segments.as_slice() {
        ["v1", category, ..] => (*category).to_string(),
        ["api", "auth", "oauth2", category, ..] => format!("oauth-{category}"),
        [".well-known", ..] => "discovery".into(),
        ["health"] => "health".into(),
        _ => "api".into(),
    }
}

/// Read the optional `x-mc-project` header (roots a connection to one container tag).
pub fn project_header(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get("x-mc-project")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

pub fn build_router(state: AppState) -> Router {
    use routes::*;

    let request_body_timeout = state.request_body_timeout;

    Router::new()
        // documents / ingestion
        .route(
            "/v1/documents",
            post(documents::ingest).get(documents::list_get),
        )
        .route("/v1/documents/batch", post(documents::batch))
        .route("/v1/documents/file", post(documents::upload_file))
        .route("/v1/documents/list", post(documents::list_post))
        .route(
            "/v1/documents/documents",
            post(documents::documents_with_memories),
        )
        .route("/v1/documents/processing", get(documents::processing))
        .route("/v1/documents/bulk", delete(documents::bulk_delete))
        .route(
            "/v1/documents/{id}",
            get(documents::get_one)
                .patch(documents::patch_one)
                .delete(documents::delete_one),
        )
        // memories
        .route(
            "/v1/memories",
            post(memories::create)
                .delete(memories::forget)
                .patch(memories::patch),
        )
        .route(
            "/v1/memories/forget-matching",
            post(memories::forget_matching),
        )
        // search
        .route(
            "/v1/documents/search",
            post(search::document_search_post).get(search::document_search_get),
        )
        .route("/v1/search", post(search::memory_search_post))
        // profile
        .route("/v1/profile", post(profile::profile))
        // projects / container tags
        .route("/v1/projects", get(projects::list).post(projects::create))
        .route("/v1/projects/{id}", delete(projects::delete))
        .route(
            "/v1/container-tags/list",
            get(projects::list).post(projects::list),
        )
        .route(
            "/v1/container-tags/{tag}",
            patch(projects::update_tag).delete(projects::delete_tag),
        )
        // settings
        .route("/v1/settings", get(settings::get).patch(settings::update))
        .route("/v1/settings/reset", post(settings::reset))
        // auth / session
        .route("/v1/session", get(auth::session))
        .route("/v1/auth/scoped-key", post(auth::create_scoped_key))
        .route("/v1/auth/scoped-key/{id}", delete(auth::revoke_scoped_key))
        // discovery / misc
        .route("/v1/openapi", get(misc::openapi))
        .route("/health", get(misc::health))
        // Phase 2/3 feature routers (self-contained sub-routers).
        .merge(routes::analytics::routes())
        .merge(routes::buckets::routes())
        .merge(routes::inferred::routes())
        .merge(routes::oauth::routes())
        .merge(routes::connections::routes())
        .merge(routes::router::routes())
        .merge(routes::admin::routes())
        .layer(DefaultBodyLimit::max(12 * 1024 * 1024))
        .layer(tower_http::timeout::RequestBodyTimeoutLayer::new(
            request_body_timeout,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            access_log,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::{apply_security_headers, request_type};
    use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, X_FRAME_OPTIONS};
    use axum::http::HeaderMap;

    #[test]
    fn access_log_categorizes_public_and_authenticated_routes() {
        assert_eq!(request_type("/v1/documents/doc_1"), "documents");
        assert_eq!(request_type("/api/auth/oauth2/token"), "oauth-token");
        assert_eq!(
            request_type("/.well-known/openid-configuration"),
            "discovery"
        );
        assert_eq!(request_type("/health"), "health");
    }

    #[test]
    fn security_headers_disable_caching_and_browser_embedding() {
        let mut headers = HeaderMap::new();
        apply_security_headers(&mut headers);
        assert_eq!(headers[CACHE_CONTROL], "no-store");
        assert_eq!(headers[X_FRAME_OPTIONS], "DENY");
        assert!(headers[CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'"));
    }
}
