//! `/v1/documents/search` + `/v1/search`.

use crate::routes::{scoped_tag, scoped_tags};
use crate::{ApiResult, AppState, Auth};
use axum::extract::{Query, State};
use axum::Json;
use memoricai_core::dto::*;
use serde::Deserialize;

pub async fn document_search_post(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(mut req): Json<DocumentSearchRequest>,
) -> ApiResult<Json<DocumentSearchResponse>> {
    req.container_tags = scoped_tags(&state, &ctx, req.container_tags.as_deref())?;
    let resp = state.engine.search_documents(&ctx.org.id, &req).await?;
    Ok(Json(resp))
}

#[derive(Debug, Deserialize)]
pub struct DocumentSearchQuery {
    q: String,
    limit: Option<u32>,
}

pub async fn document_search_get(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Query(q): Query<DocumentSearchQuery>,
) -> ApiResult<Json<DocumentSearchResponse>> {
    let req = DocumentSearchRequest {
        q: q.q,
        limit: q.limit.unwrap_or(10),
        container_tags: scoped_tags(&state, &ctx, None)?,
        filters: None,
        rerank: false,
        rewrite_query: false,
        chunk_threshold: 0.5,
        document_threshold: 0.5,
        doc_id: None,
        only_matching_chunks: true,
        include_full_docs: false,
        include_summary: false,
    };
    let resp = state.engine.search_documents(&ctx.org.id, &req).await?;
    Ok(Json(resp))
}

pub async fn memory_search_post(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(mut req): Json<MemorySearchRequest>,
) -> ApiResult<Json<MemorySearchResponse>> {
    req.container_tag = scoped_tag(&state, &ctx, req.container_tag.as_deref())?;
    let resp = state
        .engine
        .search_memories(&ctx.org.id, &req, None)
        .await?;
    Ok(Json(resp))
}
