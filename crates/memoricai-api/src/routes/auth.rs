//! `/v1/session` + scoped-key management.

use crate::{ApiError, ApiResult, AppState, Auth};
use axum::extract::{Path, State};
use axum::Json;
use memoricai_core::dto::{CreateScopedKeyRequest, CreateScopedKeyResponse, SessionResponse};
use memoricai_core::error::Error;
use serde_json::{json, Value};

pub async fn session(Auth(ctx): Auth) -> ApiResult<Json<SessionResponse>> {
    Ok(Json(SessionResponse {
        user: ctx.user,
        org: ctx.org,
    }))
}

pub async fn create_scoped_key(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<CreateScopedKeyRequest>,
) -> ApiResult<Json<CreateScopedKeyResponse>> {
    state.auth.authorize_admin(&ctx)?;
    state.auth.authorize_container(&ctx, &req.container_tag)?;
    // Lazily create the target space.
    state
        .engine
        .db
        .ensure_space(&ctx.org.id, &req.container_tag, Some(&ctx.user.id))
        .await?;
    let (key, rec) = state
        .auth
        .mint_scoped_key(
            &ctx,
            &req.container_tag,
            req.name.as_deref(),
            req.expires_in_days,
            req.rate_limit_max.unwrap_or(500),
            req.rate_limit_time_window.unwrap_or(60_000),
        )
        .await?;
    Ok(Json(CreateScopedKeyResponse {
        key,
        id: rec.id,
        name: rec.name,
        container_tag: rec.container_tag.unwrap_or_default(),
        expires_at: rec.expires_at,
        allowed_endpoints: rec.allowed_endpoints.unwrap_or_default(),
    }))
}

pub async fn revoke_scoped_key(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    let ok = state.auth.db().revoke_key(&ctx.org.id, &id).await?;
    if !ok {
        return Err(ApiError(Error::NotFound(format!("scoped key {id}"))));
    }
    Ok(Json(json!({ "success": true })))
}
