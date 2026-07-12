//! `/v1/memories/*` — direct memory CRUD + semantic forget.

use crate::routes::guard;
use crate::{ApiResult, AppState, Auth};
use axum::extract::State;
use axum::Json;
use memoricai_core::dto::*;
use memoricai_core::model::Memory;

pub async fn create(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<CreateMemoriesRequest>,
) -> ApiResult<Json<CreateMemoriesResponse>> {
    guard(&state, &ctx, "/v1/memories", Some(&req.container_tag))?;
    let resp = state
        .engine
        .create_memories(&ctx.org.id, Some(&ctx.user.id), &req)
        .await?;
    Ok(Json(resp))
}

pub async fn forget(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<ForgetRequest>,
) -> ApiResult<Json<Memory>> {
    guard(&state, &ctx, "/v1/memories", Some(&req.container_tag))?;
    let mem = state.engine.forget(&ctx.org.id, &req).await?;
    Ok(Json(mem))
}

pub async fn patch(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<PatchMemoryRequest>,
) -> ApiResult<Json<Memory>> {
    let id = req.id.as_deref().ok_or_else(|| {
        crate::ApiError(memoricai_core::Error::BadRequest("id is required".into()))
    })?;
    let target = state.engine.db.get_memory(&ctx.org.id, id).await?;
    guard(
        &state,
        &ctx,
        "/v1/memories",
        Some(&target.space_container_tag),
    )?;
    let mem = state.engine.patch_memory(&ctx.org.id, &req).await?;
    Ok(Json(mem))
}

pub async fn forget_matching(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<ForgetMatchingRequest>,
) -> ApiResult<Json<ForgetMatchingResponse>> {
    guard(&state, &ctx, "/v1/memories", Some(&req.container_tag))?;
    let resp = state.engine.forget_matching(&ctx.org.id, &req).await?;
    Ok(Json(resp))
}
