//! `/v1/analytics/*` — usage, errors, and request logs.

use crate::routes::{paginate, require_unrestricted_admin};
use crate::{ApiResult, AppState, Auth};
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use memoricai_core::dto::*;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/analytics/usage", get(usage))
        .route("/v1/analytics/errors", get(errors))
        .route("/v1/analytics/logs", get(logs))
        .route("/v1/analytics/memory", get(memory))
        .route("/v1/analytics/chat", get(chat))
}

#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    #[serde(default)]
    period: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    limit: Option<u32>,
}

fn days_for(period: Option<&str>) -> i64 {
    match period.unwrap_or("30d") {
        "1h" | "24h" => 1,
        "7d" => 7,
        "90d" => 90,
        "all" => 3650,
        _ => 30,
    }
}

pub async fn usage(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Query(q): Query<AnalyticsQuery>,
) -> ApiResult<Json<AnalyticsUsageResponse>> {
    require_unrestricted_admin(&state, &ctx)?;
    let days = days_for(q.period.as_deref());
    let (rows, total_memories) = tokio::try_join!(
        state.engine.db.usage_by_type(&ctx.org.id, days),
        state.engine.db.total_memories(&ctx.org.id)
    )?;
    let usage = rows
        .into_iter()
        .map(|r| UsageEntry {
            kind: r.kind,
            count: r.count,
            avg_duration: r.avg_duration,
        })
        .collect::<Vec<_>>();
    let count = usage.len() as u64;
    Ok(Json(AnalyticsUsageResponse {
        usage,
        total_memories,
        pagination: paginate(q.page.unwrap_or(1), q.limit.unwrap_or(20), count),
    }))
}

pub async fn errors(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Query(q): Query<AnalyticsQuery>,
) -> ApiResult<Json<AnalyticsErrorsResponse>> {
    require_unrestricted_admin(&state, &ctx)?;
    let days = days_for(q.period.as_deref());
    let (total, total_errors, by_status) = state.engine.db.error_stats(&ctx.org.id, days).await?;
    let rate = if total > 0 {
        total_errors as f64 / total as f64
    } else {
        0.0
    };
    Ok(Json(AnalyticsErrorsResponse {
        total_errors,
        error_rate: rate,
        by_status_code: by_status
            .into_iter()
            .map(|(status_code, count)| StatusCount { status_code, count })
            .collect(),
    }))
}

pub async fn logs(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    Query(q): Query<AnalyticsQuery>,
) -> ApiResult<Json<AnalyticsLogsResponse>> {
    require_unrestricted_admin(&state, &ctx)?;
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(20).min(100);
    let (rows, total) = state
        .engine
        .db
        .request_logs(&ctx.org.id, page, limit)
        .await?;
    let logs = rows
        .into_iter()
        .map(|r| LogEntry {
            id: r.id,
            created_at: r.created_at,
            kind: r.kind,
            status_code: r.status_code,
            duration: r.duration,
        })
        .collect();
    Ok(Json(AnalyticsLogsResponse {
        logs,
        pagination: paginate(page, limit, total as u64),
    }))
}

pub async fn memory(State(state): State<AppState>, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    require_unrestricted_admin(&state, &ctx)?;
    let total = state.engine.db.total_memories(&ctx.org.id).await?;
    Ok(Json(json!({ "totalMemories": total })))
}

pub async fn chat(State(state): State<AppState>, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    require_unrestricted_admin(&state, &ctx)?;
    Ok(Json(json!({ "tokensSaved": 0, "costSavedUsd": 0.0 })))
}
