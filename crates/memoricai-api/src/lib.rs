//! memoricai-api: the axum HTTP surface for `/v1`, with API-key auth,
//! tenant scoping, error shapes, and OpenAPI. `build_router` returns the app the
//! binary mounts.

pub mod routes;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{request::Parts, HeaderValue, StatusCode};
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
    /// Exact upstream origins permitted for the Memory Router. Empty = public HTTPS only.
    pub router_allowed_origins: Arc<Vec<String>>,
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
        let body = Json(serde_json::json!({
            "error": self.0.code(),
            "message": self.0.to_string(),
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
        Ok(Auth(ctx))
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
        .layer(DefaultBodyLimit::max(12 * 1024 * 1024))
        .with_state(state)
}
