//! Inferred-memory review: `/v1/container-tags/{tag}/inferred[/{memoryId}/review]`.

use crate::{ApiError, ApiResult, AppState, Auth};
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use memoricai_core::dto::{InferredListResponse, InferredMemoryDto, ReviewRequest};
use memoricai_core::error::Error;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/container-tags/{tag}/inferred", get(list))
        .route(
            "/v1/container-tags/{tag}/inferred/{memory_id}/review",
            post(review),
        )
}

pub async fn list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(tag): Path<String>,
) -> ApiResult<Json<InferredListResponse>> {
    state.auth.authorize_container(&ctx, &tag)?;
    let mems = state.engine.db.list_inferred(&ctx.org.id, &tag).await?;
    let memories: Vec<InferredMemoryDto> = mems
        .into_iter()
        .map(|m| InferredMemoryDto {
            id: m.id,
            memory: m.memory,
            parent_count: m.source_count,
            created_at: m.created_at,
            updated_at: m.updated_at,
            metadata: m.metadata,
        })
        .collect();
    let total = memories.len();
    Ok(Json(InferredListResponse { memories, total }))
}

pub async fn review(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path((tag, memory_id)): Path<(String, String)>,
    Json(req): Json<ReviewRequest>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_container(&ctx, &tag)?;
    if !matches!(req.action.as_str(), "approve" | "decline" | "undo") {
        return Err(ApiError(Error::BadRequest(
            "action must be approve|decline|undo".into(),
        )));
    }
    let updated = state
        .engine
        .db
        .review_inferred(&ctx.org.id, &tag, &memory_id, &req.action)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound("inferred memory".into())))?;
    Ok(Json(json!({
        "id": updated.id,
        "isInference": updated.is_inference,
        "isForgotten": updated.is_forgotten,
        "reviewStatus": updated.review_status,
    })))
}
