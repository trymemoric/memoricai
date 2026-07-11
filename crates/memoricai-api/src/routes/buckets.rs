//! Profile bucket definitions: `/v1/profile/buckets` (list) + `/v1/buckets` (create).

use crate::routes::scoped_tag;
use crate::{ApiError, ApiResult, AppState, Auth};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use memoricai_core::dto::BucketsResponse;
use memoricai_core::error::Error;
use memoricai_core::model::ProfileBucket;
use serde::Deserialize;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/profile/buckets", post(list_buckets))
        .route("/v1/buckets", post(create_bucket))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListBucketsReq {
    #[serde(default)]
    container_tag: Option<String>,
}

pub async fn list_buckets(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(mut req): Json<ListBucketsReq>,
) -> ApiResult<Json<BucketsResponse>> {
    req.container_tag = scoped_tag(&state, &ctx, req.container_tag.as_deref())?;
    let buckets = state
        .engine
        .db
        .list_buckets(&ctx.org.id, req.container_tag.as_deref())
        .await?;
    Ok(Json(BucketsResponse { buckets }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBucketReq {
    #[serde(default)]
    container_tag: Option<String>,
    key: String,
    description: String,
}

pub async fn create_bucket(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(mut req): Json<CreateBucketReq>,
) -> ApiResult<Json<ProfileBucket>> {
    req.container_tag = scoped_tag(&state, &ctx, req.container_tag.as_deref())?;
    if req.key.is_empty()
        || req.key.len() > 100
        || !req.key.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(ApiError(Error::BadRequest(
            "bucket key must contain 1..=100 letters, numbers, underscores, hyphens, or dots"
                .into(),
        )));
    }
    if req.description.trim().is_empty() || req.description.len() > 1000 {
        return Err(ApiError(Error::BadRequest(
            "bucket description must contain 1..=1000 bytes".into(),
        )));
    }
    let bucket = state
        .engine
        .db
        .create_bucket(
            &ctx.org.id,
            req.container_tag.as_deref(),
            &req.key,
            &req.description,
        )
        .await?;
    Ok(Json(bucket))
}
