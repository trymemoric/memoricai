//! `/v1/projects` + `/v1/container-tags/*`.

use crate::{ApiError, ApiResult, AppState, Auth};
use axum::extract::{Path, State};
use axum::Json;
use memoricai_core::dto::{CreateProjectRequest, ProjectDto, ProjectsResponse};
use memoricai_core::error::Error;
use memoricai_core::model::Space;
use serde::Deserialize;
use serde_json::{json, Value};

fn to_dto(space: Space, document_count: Option<i64>) -> ProjectDto {
    ProjectDto {
        id: space.id,
        name: space.name,
        container_tag: space.container_tag,
        emoji: space.emoji,
        created_at: space.created_at,
        updated_at: space.updated_at,
        is_experimental: space.is_experimental,
        document_count,
    }
}

pub async fn list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ApiResult<Json<ProjectsResponse>> {
    let mut spaces = state.engine.db.list_spaces(&ctx.org.id).await?;
    if let Some(allowed) = state.auth.allowed_container_tags(&ctx) {
        spaces.retain(|space| allowed.iter().any(|tag| tag == &space.container_tag));
    }
    let counts = state
        .engine
        .db
        .count_documents_by_tag(&ctx.org.id)
        .await
        .ok();
    let projects = spaces
        .into_iter()
        .map(|s| {
            let count = counts
                .as_ref()
                .map(|c| c.get(&s.container_tag).copied().unwrap_or(0));
            to_dto(s, count)
        })
        .collect();
    Ok(Json(ProjectsResponse { projects }))
}

pub async fn create(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<CreateProjectRequest>,
) -> ApiResult<Json<ProjectDto>> {
    state.auth.authorize_admin(&ctx)?;
    if state.auth.is_container_restricted(&ctx) {
        return Err(ApiError(Error::Forbidden(
            "restricted credential cannot create projects".into(),
        )));
    }
    if req.name.trim().is_empty() || req.name.len() > 100 {
        return Err(ApiError(Error::BadRequest(
            "name must be 1-100 chars".into(),
        )));
    }
    if req.emoji.as_ref().is_some_and(|emoji| emoji.len() > 32) {
        return Err(ApiError(Error::BadRequest("emoji exceeds 32 bytes".into())));
    }
    let space = state
        .engine
        .db
        .create_space(
            &ctx.org.id,
            &req.name,
            req.emoji.as_deref(),
            Some(&ctx.user.id),
        )
        .await?;
    Ok(Json(to_dto(space, Some(0))))
}

#[derive(Debug, Deserialize)]
pub struct DeleteProject {
    action: String,
    #[serde(rename = "targetProjectId")]
    target_project_id: Option<String>,
}

pub async fn delete(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(id): Path<String>,
    Json(req): Json<DeleteProject>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    let space = state
        .engine
        .db
        .get_space_by_id(&ctx.org.id, &id)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("project {id}"))))?;
    state.auth.authorize_container(&ctx, &space.container_tag)?;
    if !matches!(req.action.as_str(), "move" | "delete") {
        return Err(ApiError(Error::BadRequest(
            "action must be move or delete".into(),
        )));
    }
    let target_tag = if req.action == "move" {
        let target_id = req.target_project_id.as_deref().ok_or_else(|| {
            ApiError(Error::BadRequest(
                "targetProjectId is required when action is move".into(),
            ))
        })?;
        if target_id == id {
            return Err(ApiError(Error::BadRequest(
                "source and target projects must differ".into(),
            )));
        }
        let target = state
            .engine
            .db
            .get_space_by_id(&ctx.org.id, target_id)
            .await?
            .ok_or_else(|| ApiError(Error::NotFound(format!("project {target_id}"))))?;
        state
            .auth
            .authorize_container(&ctx, &target.container_tag)?;
        Some(target.container_tag)
    } else {
        None
    };
    let (docs, mems) = state
        .engine
        .db
        .delete_space(&ctx.org.id, &id, &req.action, target_tag.as_deref())
        .await?;
    Ok(Json(json!({
        "success": true,
        "message": format!("project {} ({})", id, req.action),
        "documentsAffected": docs,
        "memoriesAffected": mems,
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateTag {
    name: Option<String>,
    #[serde(rename = "entityContext")]
    entity_context: Option<String>,
}

pub async fn update_tag(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(tag): Path<String>,
    Json(req): Json<UpdateTag>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    state.auth.authorize_container(&ctx, &tag)?;
    if req
        .name
        .as_ref()
        .is_some_and(|name| name.trim().is_empty() || name.len() > 100)
    {
        return Err(ApiError(Error::BadRequest(
            "name must contain 1..=100 bytes".into(),
        )));
    }
    if let Some(ec) = &req.entity_context {
        if ec.len() > 1500 {
            return Err(ApiError(Error::BadRequest(
                "entityContext max 1500 chars".into(),
            )));
        }
    }
    // Ensure the space exists so per-tag settings can be stored lazily.
    state
        .engine
        .db
        .ensure_space(&ctx.org.id, &tag, Some(&ctx.user.id))
        .await?;
    let space = state
        .engine
        .db
        .update_space(
            &ctx.org.id,
            &tag,
            req.name.as_deref(),
            req.entity_context.as_deref(),
        )
        .await?;
    Ok(Json(
        serde_json::to_value(to_dto(space, None)).unwrap_or(json!({})),
    ))
}

pub async fn delete_tag(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Path(tag): Path<String>,
) -> ApiResult<Json<Value>> {
    state.auth.authorize_admin(&ctx)?;
    state.auth.authorize_container(&ctx, &tag)?;
    let space = state
        .engine
        .db
        .get_space(&ctx.org.id, &tag)
        .await?
        .ok_or_else(|| ApiError(Error::NotFound(format!("container tag {tag}"))))?;
    let (docs, mems) = state
        .engine
        .db
        .delete_space(&ctx.org.id, &space.id, "delete", None)
        .await?;
    Ok(Json(json!({
        "success": true,
        "documentsAffected": docs,
        "memoriesAffected": mems,
    })))
}
