//! `/v1/profile`.

use crate::routes::guard;
use crate::{ApiResult, AppState, Auth};
use axum::extract::State;
use axum::Json;
use memoricai_core::dto::{ProfileRequest, ProfileResponse};

pub async fn profile(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<ProfileRequest>,
) -> ApiResult<Json<ProfileResponse>> {
    guard(&state, &ctx, "/v1/profile", Some(&req.container_tag))?;
    let resp = state.engine.profile(&ctx.org.id, &req).await?;
    Ok(Json(resp))
}
