//! Route handlers grouped by resource.

pub mod analytics;
pub mod auth;
pub mod buckets;
pub mod connections;
pub mod documents;
pub mod inferred;
pub mod memories;
pub mod misc;
pub mod oauth;
pub mod profile;
pub mod projects;
pub mod router;
pub mod search;
pub mod settings;

use crate::{ApiError, AppState};
use memoricai_core::dto::Pagination;
use memoricai_core::model::AuthContext;

/// Enforce container-tag scoping for a request that operates on `tag`.
pub(crate) fn guard(
    state: &AppState,
    ctx: &AuthContext,
    path: &str,
    tag: Option<&str>,
) -> Result<(), ApiError> {
    state.auth.authorize(ctx, path, tag).map_err(ApiError)
}

pub(crate) fn guard_tags(
    state: &AppState,
    ctx: &AuthContext,
    tags: &[String],
) -> Result<(), ApiError> {
    if tags.len() > 20
        || tags
            .iter()
            .enumerate()
            .any(|(index, tag)| tags[..index].iter().any(|candidate| candidate == tag))
    {
        return Err(ApiError(memoricai_core::Error::BadRequest(
            "container tags must contain at most 20 unique tags".into(),
        )));
    }
    for tag in tags {
        state.auth.authorize_container(ctx, tag)?;
    }
    Ok(())
}

pub(crate) fn scoped_tags(
    state: &AppState,
    ctx: &AuthContext,
    tags: Option<&[String]>,
) -> Result<Option<Vec<String>>, ApiError> {
    state.auth.scope_tags(ctx, tags).map_err(ApiError)
}

pub(crate) fn scoped_tag(
    state: &AppState,
    ctx: &AuthContext,
    tag: Option<&str>,
) -> Result<Option<String>, ApiError> {
    state.auth.scope_tag(ctx, tag).map_err(ApiError)
}

pub(crate) fn paginate(page: u32, limit: u32, total: u64) -> Pagination {
    let limit = limit.max(1);
    Pagination {
        current_page: page.max(1),
        limit,
        total_items: total,
        total_pages: u32::try_from(total.div_ceil(limit as u64)).unwrap_or(u32::MAX),
    }
}
