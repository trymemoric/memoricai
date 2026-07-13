//! `/v1/admin/*` — control-plane provisioning, guarded by MEMORICAI_PROVISION_KEY.

use crate::{ApiError, ApiResult, AppState};
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use memoricai_core::error::Error;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/admin/provision", post(provision))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionRequest {
    org_name: String,
    email: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionResponse {
    org_id: String,
    org_name: String,
    user_id: String,
    /// Plaintext org API key, shown once.
    api_key: String,
}

/// Create an org + owner user + org API key for a customer signup. Meant to be
/// called by the control plane, not end users; disabled unless
/// `MEMORICAI_PROVISION_KEY` is set.
pub async fn provision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProvisionRequest>,
) -> ApiResult<(StatusCode, Json<ProvisionResponse>)> {
    guard_provision(state.provision_key.as_deref(), &headers)?;

    let org_name = req.org_name.trim();
    if org_name.is_empty() || org_name.chars().count() > 100 {
        return Err(ApiError(Error::BadRequest(
            "orgName must contain 1..=100 characters after trimming".into(),
        )));
    }
    let email = req.email.trim();
    if !email.contains('@') || email.len() > 254 {
        return Err(ApiError(Error::BadRequest(
            "email must contain '@' and be at most 254 bytes".into(),
        )));
    }

    let (org, user, api_key) = state.auth.bootstrap_org(org_name, email).await?;
    tracing::info!(org = %org.id, "provisioned org via admin endpoint");
    Ok((
        StatusCode::CREATED,
        Json(ProvisionResponse {
            org_id: org.id,
            org_name: org.name,
            user_id: user.id,
            api_key,
        }),
    ))
}

/// Guard the admin surface: a `None` key hides the endpoint's existence
/// (404) rather than revealing it needs auth; otherwise require a bearer
/// token equal to the configured key, compared in constant time.
fn guard_provision(provision_key: Option<&str>, headers: &HeaderMap) -> Result<(), Error> {
    // Belt-and-braces: `build_router` only mounts this route when
    // `provision_key` is `Some`, so this branch is unreachable in practice.
    // Kept as defense-in-depth in case that invariant is ever violated.
    let Some(expected) = provision_key else {
        return Err(Error::NotFound("not found".into()));
    };
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            // RFC 7235: auth-scheme is case-insensitive.
            let (scheme, rest) = v.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then_some(rest)
        })
        .map(str::trim)
        .ok_or_else(|| Error::Unauthorized("missing bearer token".into()))?;

    let expected = expected.as_bytes();
    let token = token.as_bytes();
    let matches = expected.len() == token.len() && bool::from(expected.ct_eq(token));
    if !matches {
        return Err(Error::Unauthorized("invalid admin credential".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        headers
    }

    #[test]
    fn disabled_when_no_key_configured() {
        let err = guard_provision(None, &headers_with_bearer("anything")).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn rejects_missing_bearer_token() {
        let err = guard_provision(Some("secret"), &HeaderMap::new()).unwrap_err();
        assert!(matches!(err, Error::Unauthorized(_)));
    }

    #[test]
    fn rejects_wrong_key_same_length() {
        let err = guard_provision(Some("secret"), &headers_with_bearer("wrongg")).unwrap_err();
        assert!(matches!(err, Error::Unauthorized(_)));
    }

    #[test]
    fn rejects_wrong_length_key() {
        let err = guard_provision(Some("secret"), &headers_with_bearer("short")).unwrap_err();
        assert!(matches!(err, Error::Unauthorized(_)));
    }

    #[test]
    fn accepts_correct_key() {
        guard_provision(Some("secret"), &headers_with_bearer("secret")).expect("should pass");
    }
}
