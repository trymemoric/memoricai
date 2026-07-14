//! `/v1/settings`.

use crate::routes::require_unrestricted_admin;
use crate::{ApiError, ApiResult, AppState, Auth};
use axum::extract::State;
use axum::Json;
use memoricai_core::dto::UpdateSettingsRequest;
use memoricai_core::error::Error;
use memoricai_core::model::OrgSettings;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn get(State(state): State<AppState>, Auth(ctx): Auth) -> ApiResult<Json<OrgSettings>> {
    require_unrestricted_admin(&state, &ctx)?;
    let settings = state.engine.db.get_settings(&ctx.org.id).await?;
    Ok(Json(settings))
}

pub async fn update(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<UpdateSettingsRequest>,
) -> ApiResult<Json<OrgSettings>> {
    require_unrestricted_admin(&state, &ctx)?;
    // `shouldLLMFilter` is a master switch gating filter/categories/include/exclude.
    let filter_fields_present = req.filter_prompt.is_some()
        || req.categories.is_some()
        || req.include_items.is_some()
        || req.exclude_items.is_some();
    let current = state.engine.db.get_settings(&ctx.org.id).await?;
    let effective = req.should_llm_filter.unwrap_or(current.should_llm_filter);
    if filter_fields_present && !effective {
        return Err(ApiError(Error::BadRequest(
            "shouldLLMFilter must be enabled to set filter/categories/include/exclude".into(),
        )));
    }
    if let Some(fp) = &req.filter_prompt {
        if fp.trim().is_empty() || fp.len() > 750 {
            return Err(ApiError(Error::BadRequest(
                "filterPrompt must contain 1..=750 bytes".into(),
            )));
        }
    }
    for (name, items) in [
        ("categories", req.categories.as_deref()),
        ("includeItems", req.include_items.as_deref()),
        ("excludeItems", req.exclude_items.as_deref()),
    ] {
        if let Some(items) = items {
            if items.len() > 100
                || items
                    .iter()
                    .any(|item| item.trim().is_empty() || item.len() > 200)
            {
                return Err(ApiError(Error::BadRequest(format!(
                    "{name} accepts at most 100 non-empty items of at most 200 bytes"
                ))));
            }
        }
    }
    if let Some(chunk_size) = req.chunk_size {
        if chunk_size != -1 && !(200..=20_000).contains(&chunk_size) {
            return Err(ApiError(Error::BadRequest(
                "chunkSize must be -1 (default) or between 200 and 20000".into(),
            )));
        }
    }
    let updated = state.engine.db.update_settings(&ctx.org.id, &req).await?;
    Ok(Json(updated))
}

#[derive(Debug, Deserialize)]
pub struct ResetReq {
    confirmation: Option<String>,
}

pub async fn reset(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Json(req): Json<ResetReq>,
) -> ApiResult<Json<Value>> {
    require_unrestricted_admin(&state, &ctx)?;
    if req.confirmation.as_deref() != Some("RESET") {
        return Err(ApiError(Error::BadRequest(
            "confirmation must be \"RESET\"".into(),
        )));
    }
    let (docs, mems) = state.engine.db.reset_org_data(&ctx.org.id).await?;
    Ok(Json(json!({
        "success": true,
        "documentsDeleted": docs,
        "memoriesDeleted": mems,
    })))
}
