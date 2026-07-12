//! `/v1/documents/*` — ingestion + document CRUD.

use crate::routes::{guard_tags, paginate, scoped_tags};
use crate::{ApiError, ApiResult, AppState, Auth};
use axum::extract::{Multipart, Path, Query, State};
use axum::Json;
use memoricai_core::dto::*;
use memoricai_core::error::Error;
use serde::Deserialize;
use serde_json::{json, Value};

fn ingest_response(id: String, status: memoricai_core::enums::DocumentStatus) -> IngestResponse {
    IngestResponse {
        id,
        status: status.as_str().to_string(),
    }
}

fn scope_ingest_request(
    state: &AppState,
    ctx: &memoricai_core::model::AuthContext,
    req: &mut IngestRequest,
) -> ApiResult<()> {
    if req.resolved_container_tags().is_empty() {
        if let Some(tags) = scoped_tags(state, ctx, None)? {
            req.container_tags = Some(tags);
        }
    }
    guard_tags(state, ctx, &req.resolved_container_tags())
}

pub async fn ingest(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(mut req): Json<IngestRequest>,
) -> ApiResult<Json<IngestResponse>> {
    scope_ingest_request(&state, &ctx, &mut req)?;
    let allowed_tags = state.auth.allowed_container_tags(&ctx);
    let (id, status) = state
        .engine
        .ingest_scoped(
            &ctx.org.id,
            Some(&ctx.user.id),
            &req,
            allowed_tags.as_deref(),
        )
        .await?;
    Ok(Json(ingest_response(id, status)))
}

pub async fn batch(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<BatchIngestRequest>,
) -> ApiResult<Json<BatchIngestResponse>> {
    if req.documents.is_empty() || req.documents.len() > 600 {
        return Err(ApiError(Error::BadRequest(
            "batch must contain between 1 and 600 documents".into(),
        )));
    }
    let mut results = Vec::with_capacity(req.documents.len());
    let mut success = 0;
    let mut failed = 0;
    let allowed_tags = state.auth.allowed_container_tags(&ctx);
    for mut doc in req.documents {
        // Apply batch-level defaults.
        if doc.container_tag.is_none() && doc.container_tags.is_none() {
            doc.container_tag = req.container_tag.clone();
        }
        if doc.entity_context.is_none() {
            doc.entity_context = req.entity_context.clone();
        }
        if doc.metadata.is_none() {
            doc.metadata = req.metadata.clone();
        }
        if let Err(e) = scope_ingest_request(&state, &ctx, &mut doc) {
            failed += 1;
            results.push(BatchIngestItem {
                id: None,
                status: "failed".into(),
                error: Some(e.0.to_string()),
            });
            continue;
        }
        match state
            .engine
            .ingest_scoped(
                &ctx.org.id,
                Some(&ctx.user.id),
                &doc,
                allowed_tags.as_deref(),
            )
            .await
        {
            Ok((id, status)) => {
                success += 1;
                results.push(BatchIngestItem {
                    id: Some(id),
                    status: status.as_str().to_string(),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(BatchIngestItem {
                    id: None,
                    status: "failed".into(),
                    error: Some(e.to_string()),
                });
            }
        }
    }
    Ok(Json(BatchIngestResponse {
        results,
        success,
        failed,
    }))
}

pub async fn upload_file(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    mut mp: Multipart,
) -> ApiResult<Json<IngestResponse>> {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut mime = String::new();
    let mut tags: Vec<String> = Vec::new();
    let mut metadata: Value = json!({});

    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| ApiError(Error::BadRequest(e.to_string())))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                if file_bytes.is_some() {
                    return Err(ApiError(Error::BadRequest(
                        "only one file field is allowed".into(),
                    )));
                }
                filename = field.file_name().map(|s| s.to_string());
                mime = field.content_type().unwrap_or("").to_string();
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError(Error::BadRequest(e.to_string())))?;
                file_bytes = Some(data.to_vec());
            }
            "containerTags" | "containerTag" => {
                if let Ok(v) = field.text().await {
                    tags.push(v);
                }
            }
            "metadata" => {
                if let Ok(v) = field.text().await {
                    metadata = serde_json::from_str(&v).map_err(|error| {
                        ApiError(Error::BadRequest(format!("invalid metadata JSON: {error}")))
                    })?;
                }
            }
            _ => {}
        }
    }

    let bytes =
        file_bytes.ok_or_else(|| ApiError(Error::BadRequest("missing file field".into())))?;
    if bytes.is_empty() || bytes.len() > memoricai_engine::MAX_DOCUMENT_BYTES {
        return Err(ApiError(Error::BadRequest(
            "file must contain between 1 byte and 10 MiB".into(),
        )));
    }
    let fname = filename.unwrap_or_else(|| "upload".to_string());
    if fname.len() > 512 || fname.chars().any(char::is_control) {
        return Err(ApiError(Error::BadRequest(
            "filename must be at most 512 printable bytes".into(),
        )));
    }
    let (content, doc_type) = state.engine.extract_file(&bytes, &fname, &mime).await?;

    let mut req = IngestRequest {
        content,
        custom_id: None,
        // Leave the singular tag unset so ALL uploaded container tags flow through
        // (resolved_container_tags prefers the singular and would otherwise drop the rest).
        container_tag: None,
        container_tags: if tags.is_empty() { None } else { Some(tags) },
        metadata: Some(metadata),
        entity_context: None,
        content_type: Some(doc_type),
        title: Some(fname),
        raw: None,
    };
    scope_ingest_request(&state, &ctx, &mut req)?;
    let allowed_tags = state.auth.allowed_container_tags(&ctx);
    let (id, status) = state
        .engine
        .ingest_scoped(
            &ctx.org.id,
            Some(&ctx.user.id),
            &req,
            allowed_tags.as_deref(),
        )
        .await?;
    Ok(Json(ingest_response(id, status)))
}

pub async fn list_post(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<DocumentListRequest>,
) -> ApiResult<Json<DocumentListResponse>> {
    let tags = scoped_tags(&state, &ctx, req.container_tags.as_deref())?;
    list_impl(
        &state,
        &ctx.org.id,
        req.page.unwrap_or(1),
        req.limit.unwrap_or(50),
        tags.as_deref(),
        req.status.as_deref(),
        req.sort.as_deref().unwrap_or("createdAt"),
        req.order.as_deref().unwrap_or("desc"),
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    page: Option<u32>,
    limit: Option<u32>,
    #[serde(rename = "containerTags")]
    container_tags: Option<String>,
    status: Option<String>,
}

pub async fn list_get(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<DocumentListResponse>> {
    let requested: Option<Vec<String>> = q
        .container_tags
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect());
    let tags = scoped_tags(&state, &ctx, requested.as_deref())?;
    list_impl(
        &state,
        &ctx.org.id,
        q.page.unwrap_or(1),
        q.limit.unwrap_or(50),
        tags.as_deref(),
        q.status.as_deref(),
        "createdAt",
        "desc",
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn list_impl(
    state: &AppState,
    org_id: &str,
    page: u32,
    limit: u32,
    tags: Option<&[String]>,
    status: Option<&str>,
    sort: &str,
    order: &str,
) -> ApiResult<Json<DocumentListResponse>> {
    if page == 0 || limit == 0 || limit > 200 {
        return Err(ApiError(Error::BadRequest(
            "page must be positive and limit must be between 1 and 200".into(),
        )));
    }
    if status.is_some_and(|status| {
        memoricai_core::enums::DocumentStatus::parse(status)
            == memoricai_core::enums::DocumentStatus::Unknown
    }) {
        return Err(ApiError(Error::BadRequest(
            "invalid document status".into(),
        )));
    }
    let (docs, total) = state
        .engine
        .db
        .list_documents(org_id, tags, status, page, limit, sort, order)
        .await?;
    Ok(Json(DocumentListResponse {
        memories: docs,
        pagination: paginate(page, limit, total),
    }))
}

pub async fn documents_with_memories(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<DocumentListRequest>,
) -> ApiResult<Json<Value>> {
    let page = req.page.unwrap_or(1);
    let limit = req.limit.unwrap_or(10);
    if page == 0 || limit == 0 || limit > 100 {
        return Err(ApiError(Error::BadRequest(
            "page must be positive and limit must be between 1 and 100".into(),
        )));
    }
    let tags = scoped_tags(&state, &ctx, req.container_tags.as_deref())?;
    let (docs, total) = state
        .engine
        .db
        .list_documents(
            &ctx.org.id,
            tags.as_deref(),
            None,
            page,
            limit,
            "createdAt",
            "desc",
        )
        .await?;
    let doc_ids: Vec<String> = docs.iter().map(|doc| doc.id.clone()).collect();
    let mut mems_by_doc = state
        .engine
        .db
        .memories_for_documents(&doc_ids)
        .await
        .unwrap_or_default();
    let mut out = Vec::with_capacity(docs.len());
    for doc in &docs {
        let mems = mems_by_doc.remove(&doc.id).unwrap_or_default();
        let mut v = serde_json::to_value(doc).unwrap_or(json!({}));
        v["memoryEntries"] = serde_json::to_value(&mems).unwrap_or(json!([]));
        out.push(v);
    }
    Ok(Json(json!({
        "documents": out,
        "pagination": paginate(page, limit, total),
    })))
}

pub async fn processing(State(state): State<AppState>, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    let tags = scoped_tags(&state, &ctx, None)?;
    let docs = state
        .engine
        .db
        .list_processing(&ctx.org.id, tags.as_deref())
        .await?;
    Ok(Json(json!({ "documents": docs })))
}

pub async fn get_one(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<memoricai_core::model::Document>> {
    let doc = state.engine.db.get_document(&ctx.org.id, &id).await?;
    state
        .auth
        .authorize_resource_tags(&ctx, &doc.container_tags)?;
    Ok(Json(doc))
}

#[derive(Debug, Deserialize)]
pub struct PatchDoc {
    content: Option<String>,
    metadata: Option<Value>,
}

pub async fn patch_one(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
    Json(req): Json<PatchDoc>,
) -> ApiResult<Json<memoricai_core::model::Document>> {
    let existing = state.engine.db.get_document(&ctx.org.id, &id).await?;
    state
        .auth
        .authorize_resource_write_tags(&ctx, &existing.container_tags)?;
    let allowed_tags = state.auth.allowed_container_tags(&ctx);
    let doc = state
        .engine
        .patch_document(
            &ctx.org.id,
            &id,
            req.content.as_deref(),
            req.metadata.as_ref(),
            allowed_tags.as_deref(),
        )
        .await?;
    Ok(Json(doc))
}

pub async fn delete_one(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let existing = state.engine.db.get_document(&ctx.org.id, &id).await?;
    state
        .auth
        .authorize_resource_write_tags(&ctx, &existing.container_tags)?;
    let allowed_tags = state.auth.allowed_container_tags(&ctx);
    let deleted = state
        .engine
        .db
        .delete_document(&ctx.org.id, &id, allowed_tags.as_deref())
        .await?;
    if !deleted {
        return Err(ApiError(Error::NotFound(format!("document {id}"))));
    }
    Ok(Json(json!({ "success": true, "id": id })))
}

#[derive(Debug, Deserialize)]
pub struct BulkDelete {
    ids: Option<Vec<String>>,
    #[serde(rename = "containerTags")]
    container_tags: Option<Vec<String>>,
}

pub async fn bulk_delete(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<BulkDelete>,
) -> ApiResult<Json<BulkDeleteResponse>> {
    let count = if let Some(ids) = req.ids {
        if ids.is_empty() || ids.len() > 100 {
            return Err(ApiError(Error::BadRequest("ids must be 1-100".into())));
        }
        let docs = state.engine.db.documents_by_ids(&ctx.org.id, &ids).await?;
        for doc in &docs {
            state
                .auth
                .authorize_resource_write_tags(&ctx, &doc.container_tags)?;
        }
        let allowed_tags = state.auth.allowed_container_tags(&ctx);
        state
            .engine
            .db
            .bulk_delete_by_ids(&ctx.org.id, &ids, allowed_tags.as_deref())
            .await?
    } else if let Some(tags) = req.container_tags {
        guard_tags(&state, &ctx, &tags)?;
        let allowed_tags = state.auth.allowed_container_tags(&ctx);
        state
            .engine
            .db
            .bulk_delete_by_tags(&ctx.org.id, &tags, allowed_tags.as_deref())
            .await?
    } else {
        return Err(ApiError(Error::BadRequest(
            "provide ids or containerTags".into(),
        )));
    };
    Ok(Json(BulkDeleteResponse {
        success: true,
        deleted_count: count,
    }))
}
