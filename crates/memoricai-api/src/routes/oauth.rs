//! OAuth2 / OIDC provider endpoints + the MCP token→key exchange.
//! Opaque access/refresh tokens (DB-introspected), PKCE, and dynamic client
//! registration. Because this is a headless self-host, the authorize page asks
//! the user to paste an API key to authenticate the consent.

use crate::{ApiError, ApiResult, AppState, Auth, RequestLog};
use axum::extract::{DefaultBodyLimit, Extension, Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use memoricai_auth::oauth::{opaque_token, verify_pkce};
use memoricai_core::dto::{
    RegisterClientRequest, RegisterClientResponse, SessionWithKeyResponse, TokenResponse,
};
use memoricai_core::error::Error;
use memoricai_db::oauth::{OAuthClient, OAuthCode, OAuthToken};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/oauth2/authorize", get(authorize))
        .route("/api/auth/oauth2/consent", post(consent))
        .route("/api/auth/oauth2/token", post(token))
        .route("/api/auth/oauth2/register", post(register))
        .route("/v1/mcp/session-with-key", get(session_with_key))
        .route("/v1/mcp/connect-scope", post(connect_scope))
        .route("/.well-known/oauth-authorization-server", get(as_metadata))
        .route("/.well-known/openid-configuration", get(as_metadata))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

fn base_url(headers: &HeaderMap) -> ApiResult<String> {
    let configured = std::env::var("MEMORICAI_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let candidate = configured.clone().unwrap_or_else(|| {
        let host = headers
            .get("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("localhost:7373");
        format!("http://{host}")
    });
    let url = reqwest::Url::parse(&candidate)
        .map_err(|_| ApiError(Error::BadRequest("invalid MEMORICAI_BASE_URL".into())))?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1")
    );
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
    {
        return Err(ApiError(Error::BadRequest(
            "MEMORICAI_BASE_URL must be HTTPS (or loopback HTTP) without credentials, query, or fragment"
                .into(),
        )));
    }
    if configured.is_none() && !loopback {
        return Err(ApiError(Error::BadRequest(
            "MEMORICAI_BASE_URL is required for non-loopback OAuth discovery".into(),
        )));
    }
    Ok(candidate.trim_end_matches('/').to_string())
}

fn valid_redirect_uri(uri: &str) -> bool {
    if uri.len() > 2048 {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(uri) else {
        return false;
    };
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        return false;
    }
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    url.scheme() == "https" || (url.scheme() == "http" && loopback)
}

fn redirect_registered(client: &OAuthClient, uri: &str) -> bool {
    valid_redirect_uri(uri)
        && client
            .redirect_uris
            .iter()
            .any(|registered| registered == uri)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn validate_pkce_request(
    public_client: bool,
    challenge: Option<&str>,
    method: Option<&str>,
) -> Result<(), ApiError> {
    if public_client && challenge.is_none_or(str::is_empty) {
        return Err(ApiError(Error::BadRequest(
            "public clients must use PKCE".into(),
        )));
    }
    if challenge.is_some() && method.unwrap_or("S256") != "S256" {
        return Err(ApiError(Error::BadRequest(
            "only PKCE S256 is supported".into(),
        )));
    }
    if challenge.is_some_and(|challenge| {
        challenge.len() != 43
            || !challenge
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }) {
        return Err(ApiError(Error::BadRequest(
            "invalid PKCE S256 code_challenge".into(),
        )));
    }
    Ok(())
}

fn valid_code_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

// ---------------- authorize (renders consent page) ----------------

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    #[serde(default)]
    #[allow(dead_code)]
    response_type: Option<String>,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
}

pub async fn authorize(
    State(state): State<AppState>,
    Query(q): Query<AuthorizeQuery>,
) -> ApiResult<Html<String>> {
    if q.client_id.len() > 255
        || q.redirect_uri.len() > 2048
        || q.scope.as_ref().is_some_and(|value| value.len() > 1024)
        || q.state.as_ref().is_some_and(|value| value.len() > 1024)
        || q.response_type
            .as_ref()
            .is_some_and(|value| value.len() > 32)
        || q.code_challenge
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        || q.code_challenge_method
            .as_ref()
            .is_some_and(|value| value.len() > 32)
    {
        return Err(ApiError(Error::BadRequest(
            "OAuth request parameter exceeds its size limit".into(),
        )));
    }
    let client = state
        .auth
        .db()
        .get_oauth_client(&q.client_id)
        .await?
        .ok_or_else(|| ApiError(Error::BadRequest("unknown client_id".into())))?;
    if q.response_type.as_deref().unwrap_or("code") != "code" {
        return Err(ApiError(Error::BadRequest(
            "unsupported response_type".into(),
        )));
    }
    if !client
        .grant_types
        .iter()
        .any(|grant| grant == "authorization_code")
    {
        return Err(ApiError(Error::BadRequest(
            "client is not registered for authorization_code".into(),
        )));
    }
    if !redirect_registered(&client, &q.redirect_uri) {
        return Err(ApiError(Error::BadRequest(
            "redirect_uri not registered".into(),
        )));
    }
    validate_pkce_request(
        client.client_secret.is_none(),
        q.code_challenge.as_deref(),
        q.code_challenge_method.as_deref(),
    )?;
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let hidden = |name: &str, val: &str| {
        format!(
            r#"<input type="hidden" name="{name}" value="{}">"#,
            esc(val)
        )
    };
    let page = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Authorize {app}</title>
<style>body{{font-family:system-ui;max-width:28rem;margin:4rem auto;padding:1rem}}
input,button,select{{width:100%;padding:.6rem;margin:.3rem 0;box-sizing:border-box}}
button{{background:#3b73b8;color:#fff;border:0;border-radius:.4rem;cursor:pointer}}</style></head>
<body><h2>Authorize “{app}”</h2>
<p>“{app}” is requesting access to your memoricai memories. Paste an API key to approve.</p>
<form method="post" action="/api/auth/oauth2/consent">
<input type="password" name="api_key" placeholder="mc_..." autocomplete="off" required>
<input type="text" name="container_tags" placeholder="container tags (comma-separated, optional)">
<label>Permission <select name="permission"><option value="write">read + write</option><option value="read">read only</option></select></label>
{h_client}{h_redirect}{h_scope}{h_state}{h_cc}{h_ccm}
<button type="submit">Approve</button></form></body></html>"#,
        app = esc(&client.name),
        h_client = hidden("client_id", &q.client_id),
        h_redirect = hidden("redirect_uri", &q.redirect_uri),
        h_scope = hidden("scope", q.scope.as_deref().unwrap_or("")),
        h_state = hidden("state", q.state.as_deref().unwrap_or("")),
        h_cc = hidden("code_challenge", q.code_challenge.as_deref().unwrap_or("")),
        h_ccm = hidden(
            "code_challenge_method",
            q.code_challenge_method.as_deref().unwrap_or("")
        ),
    );
    Ok(Html(page))
}

// ---------------- consent (issues code) ----------------

#[derive(Debug, Deserialize)]
pub struct ConsentForm {
    api_key: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    #[serde(default)]
    container_tags: String,
    #[serde(default)]
    permission: String,
}

pub(crate) async fn consent(
    State(state): State<AppState>,
    Extension(request_log): Extension<RequestLog>,
    Form(f): Form<ConsentForm>,
) -> ApiResult<Response> {
    if f.permission != "read" && f.permission != "write" {
        return Err(ApiError(Error::BadRequest(
            "permission must be read or write".into(),
        )));
    }
    if f.api_key.len() > 512
        || f.client_id.len() > 255
        || f.redirect_uri.len() > 2048
        || f.scope.len() > 1024
        || f.state.len() > 1024
        || f.container_tags.len() > 4096
        || f.code_challenge.len() > 128
        || f.code_challenge_method.len() > 32
    {
        return Err(ApiError(Error::BadRequest(
            "OAuth consent parameter exceeds its size limit".into(),
        )));
    }
    let ctx = state.auth.introspect(f.api_key.trim()).await?;
    request_log.set(&ctx);
    let client = state
        .auth
        .db()
        .get_oauth_client(&f.client_id)
        .await?
        .ok_or_else(|| ApiError(Error::BadRequest("unknown client_id".into())))?;
    if !redirect_registered(&client, &f.redirect_uri) {
        return Err(ApiError(Error::BadRequest(
            "redirect_uri not registered".into(),
        )));
    }
    validate_pkce_request(
        client.client_secret.is_none(),
        (!f.code_challenge.is_empty()).then_some(f.code_challenge.as_str()),
        (!f.code_challenge_method.is_empty()).then_some(f.code_challenge_method.as_str()),
    )?;
    let requested_tags: Vec<String> = f
        .container_tags
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if requested_tags.len() > 20
        || requested_tags
            .iter()
            .any(|tag| !memoricai_core::is_valid_container_tag(tag))
        || requested_tags.iter().enumerate().any(|(index, tag)| {
            requested_tags[..index]
                .iter()
                .any(|candidate| candidate == tag)
        })
    {
        return Err(ApiError(Error::BadRequest(
            "container_tags must contain at most 20 valid tags".into(),
        )));
    }
    let tags = state
        .auth
        .scope_tags(
            &ctx,
            (!requested_tags.is_empty()).then_some(requested_tags.as_slice()),
        )?
        .unwrap_or_default();
    let permission = if f.permission == "read" || ctx.permission == "read" {
        "read"
    } else {
        "write"
    };
    let code = opaque_token();
    let oauth_code = OAuthCode {
        code: code.clone(),
        client_id: client.id,
        user_id: ctx.user.id.clone(),
        org_id: ctx.org.id.clone(),
        redirect_uri: f.redirect_uri.clone(),
        code_challenge: (!f.code_challenge.is_empty()).then_some(f.code_challenge),
        code_challenge_method: (!f.code_challenge_method.is_empty())
            .then_some(f.code_challenge_method),
        scope: (!f.scope.is_empty()).then_some(f.scope),
        container_tags: tags,
        permission: permission.into(),
        expires_at: Utc::now() + Duration::minutes(10),
    };
    state.auth.db().insert_oauth_code(&oauth_code).await?;
    let sep = if f.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let redirect = format!(
        "{}{}code={}&state={}",
        f.redirect_uri,
        sep,
        memoricai_connectors::oauth::urlencode(&code),
        memoricai_connectors::oauth::urlencode(&f.state)
    );
    Ok(Redirect::to(&redirect).into_response())
}

// ---------------- token ----------------

#[derive(Debug, Deserialize)]
pub struct TokenForm {
    grant_type: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

async fn authenticate_client(
    state: &AppState,
    client_id: &str,
    client_secret: Option<&str>,
    grant_type: &str,
) -> Result<OAuthClient, ApiError> {
    let client = state
        .auth
        .db()
        .get_oauth_client(client_id)
        .await?
        .ok_or_else(|| ApiError(Error::Unauthorized("invalid_client".into())))?;
    if !client.grant_types.iter().any(|grant| grant == grant_type) {
        return Err(ApiError(Error::BadRequest(
            "client is not registered for this grant".into(),
        )));
    }
    if let Some(expected) = &client.client_secret {
        let supplied =
            client_secret.ok_or_else(|| ApiError(Error::Unauthorized("invalid_client".into())))?;
        // The stored secret is a SHA-256 hash.
        let hashed = memoricai_db::crypto::hash_token(supplied);
        if !constant_time_eq(expected, &hashed) {
            return Err(ApiError(Error::Unauthorized("invalid_client".into())));
        }
    }
    Ok(client)
}

pub async fn token(
    State(state): State<AppState>,
    Form(f): Form<TokenForm>,
) -> ApiResult<Json<TokenResponse>> {
    if f.grant_type.len() > 64
        || f.client_id.len() > 255
        || f.client_secret
            .as_ref()
            .is_some_and(|value| value.len() > 512)
        || f.code.as_ref().is_some_and(|value| value.len() > 512)
        || f.redirect_uri
            .as_ref()
            .is_some_and(|value| value.len() > 2048)
        || f.code_verifier
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        || f.refresh_token
            .as_ref()
            .is_some_and(|value| value.len() > 512)
    {
        return Err(ApiError(Error::BadRequest(
            "OAuth token parameter exceeds its size limit".into(),
        )));
    }
    let client = authenticate_client(
        &state,
        &f.client_id,
        f.client_secret.as_deref(),
        &f.grant_type,
    )
    .await?;
    match f.grant_type.as_str() {
        "authorization_code" => {
            let code = f
                .code
                .ok_or_else(|| ApiError(Error::BadRequest("missing code".into())))?;
            let oc = state
                .auth
                .db()
                .get_oauth_code(&code)
                .await?
                .ok_or_else(|| ApiError(Error::BadRequest("invalid_grant".into())))?;
            if oc.client_id != client.id {
                return Err(ApiError(Error::BadRequest("invalid_grant".into())));
            }
            if oc.expires_at < Utc::now() {
                return Err(ApiError(Error::BadRequest("expired code".into())));
            }
            if f.redirect_uri.as_deref() != Some(oc.redirect_uri.as_str()) {
                return Err(ApiError(Error::BadRequest("redirect_uri mismatch".into())));
            }
            if let Some(challenge) = &oc.code_challenge {
                let verifier = f
                    .code_verifier
                    .ok_or_else(|| ApiError(Error::BadRequest("missing code_verifier".into())))?;
                if !valid_code_verifier(&verifier)
                    || !verify_pkce(&verifier, challenge, oc.code_challenge_method.as_deref())
                {
                    return Err(ApiError(Error::BadRequest("invalid PKCE verifier".into())));
                }
            } else if client.client_secret.is_none() {
                return Err(ApiError(Error::BadRequest("PKCE required".into())));
            }
            let consumed = state
                .auth
                .db()
                .take_oauth_code(&code, &client.id)
                .await?
                .ok_or_else(|| ApiError(Error::BadRequest("invalid_grant".into())))?;
            Ok(Json(mint_token(&state, &consumed).await?))
        }
        "refresh_token" => {
            let refresh = f
                .refresh_token
                .ok_or_else(|| ApiError(Error::BadRequest("missing refresh_token".into())))?;
            let old = state
                .auth
                .db()
                .take_oauth_token_by_refresh(&refresh, &client.id)
                .await?
                .ok_or_else(|| ApiError(Error::BadRequest("invalid_grant".into())))?;
            if let Some(exp) = old.refresh_expires_at {
                if exp < Utc::now() {
                    return Err(ApiError(Error::BadRequest("refresh token expired".into())));
                }
            }
            let rotated = OAuthCode {
                code: String::new(),
                client_id: old.client_id.clone(),
                user_id: old.user_id.clone(),
                org_id: old.org_id.clone(),
                redirect_uri: String::new(),
                code_challenge: None,
                code_challenge_method: None,
                scope: old.scope.clone(),
                container_tags: old.container_tags.clone(),
                permission: old.permission.clone(),
                expires_at: Utc::now(),
            };
            Ok(Json(mint_token(&state, &rotated).await?))
        }
        other => Err(ApiError(Error::BadRequest(format!(
            "unsupported grant_type: {other}"
        )))),
    }
}

async fn mint_token(state: &AppState, oc: &OAuthCode) -> Result<TokenResponse, ApiError> {
    let access = opaque_token();
    let refresh = opaque_token();
    let tok = OAuthToken {
        access_token: access.clone(),
        refresh_token: Some(refresh.clone()),
        client_id: oc.client_id.clone(),
        user_id: oc.user_id.clone(),
        org_id: oc.org_id.clone(),
        container_tags: oc.container_tags.clone(),
        scope: oc.scope.clone(),
        permission: oc.permission.clone(),
        access_expires_at: Utc::now() + Duration::hours(1),
        refresh_expires_at: Some(Utc::now() + Duration::days(30)),
        revoked: false,
    };
    state.auth.db().insert_oauth_token(&tok).await?;
    Ok(TokenResponse {
        access_token: access,
        token_type: "Bearer".into(),
        expires_in: 3600,
        refresh_token: Some(refresh),
        scope: oc.scope.clone(),
    })
}

// ---------------- dynamic client registration ----------------

/// Global fixed-window rate limit for the unauthenticated dynamic-registration
/// endpoint, so it cannot be used to cheaply fill `oauth_clients`.
fn register_rate_ok() -> bool {
    static WINDOW: std::sync::LazyLock<std::sync::Mutex<(i64, u32)>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new((0, 0)));
    const WINDOW_MS: i64 = 60_000;
    const MAX_PER_WINDOW: u32 = 20;
    let now = Utc::now().timestamp_millis();
    let mut w = WINDOW.lock().unwrap();
    if now - w.0 >= WINDOW_MS {
        *w = (now, 0);
    }
    w.1 += 1;
    w.1 <= MAX_PER_WINDOW
}

/// Hard ceiling on dynamically-registered clients (defence in depth vs. the rate limit).
const MAX_DYNAMIC_OAUTH_CLIENTS: i64 = 10_000;

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterClientRequest>,
) -> ApiResult<Json<RegisterClientResponse>> {
    if !register_rate_ok() {
        return Err(ApiError(Error::RateLimited));
    }
    if state.auth.db().count_dynamic_oauth_clients().await? >= MAX_DYNAMIC_OAUTH_CLIENTS {
        return Err(ApiError(Error::Forbidden(
            "dynamic client registration is at capacity".into(),
        )));
    }
    let auth_method = req
        .token_endpoint_auth_method
        .unwrap_or_else(|| "none".into());
    if !matches!(auth_method.as_str(), "none" | "client_secret_post") {
        return Err(ApiError(Error::BadRequest(
            "unsupported token_endpoint_auth_method".into(),
        )));
    }
    if req.redirect_uris.is_empty()
        || req.redirect_uris.len() > 10
        || !req.redirect_uris.iter().all(|uri| valid_redirect_uri(uri))
    {
        return Err(ApiError(Error::BadRequest(
            "redirect_uris must contain 1-10 HTTPS or loopback HTTP URLs".into(),
        )));
    }
    let public = auth_method == "none";
    let grant_types = req
        .grant_types
        .unwrap_or_else(|| vec!["authorization_code".into(), "refresh_token".into()]);
    if grant_types.is_empty()
        || !grant_types
            .iter()
            .all(|grant| matches!(grant.as_str(), "authorization_code" | "refresh_token"))
    {
        return Err(ApiError(Error::BadRequest(
            "unsupported grant_types".into(),
        )));
    }
    let name = req.client_name.unwrap_or_else(|| "mcp-client".into());
    if name.trim().is_empty() || name.len() > 100 {
        return Err(ApiError(Error::BadRequest(
            "client_name must be 1-100 characters".into(),
        )));
    }
    let client = OAuthClient {
        id: format!("client_{}", memoricai_core::ids::token(20)),
        client_secret: (!public).then(|| memoricai_core::ids::token(40)),
        name,
        redirect_uris: req.redirect_uris,
        grant_types: grant_types.clone(),
        first_party: false,
    };
    state.auth.db().insert_oauth_client(&client).await?;
    Ok(Json(RegisterClientResponse {
        client_id: client.id,
        client_secret: client.client_secret,
        redirect_uris: client.redirect_uris,
        grant_types,
        token_endpoint_auth_method: auth_method,
    }))
}

// ---------------- MCP token exchange ----------------

pub(crate) async fn session_with_key(
    State(state): State<AppState>,
    Extension(request_log): Extension<RequestLog>,
    headers: HeaderMap,
) -> ApiResult<Json<SessionWithKeyResponse>> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .ok_or_else(|| ApiError(Error::Unauthorized("missing bearer token".into())))?;
    let ctx = state.auth.introspect_oauth(token.trim()).await?;
    request_log.set(&ctx);
    state.auth.authorize_write(&ctx)?;
    let api_key = match state.auth.allowed_container_tags(&ctx) {
        None => {
            // A bounded, revocable, rate-limited session key — never a permanent org key.
            state
                .auth
                .mint_session_key(&ctx.org.id, Some(&ctx.user.id), "mcp", 30, 500, 60_000)
                .await?
        }
        Some(tags) if tags.len() == 1 => {
            state
                .auth
                .mint_scoped_key(&ctx, &tags[0], Some("mcp"), Some(30), 500, 60_000)
                .await?
                .0
        }
        Some(_) => {
            return Err(ApiError(Error::BadRequest(
                "cannot exchange a multi-container OAuth token for one API key".into(),
            )))
        }
    };
    Ok(Json(SessionWithKeyResponse {
        user_id: ctx.user.id.clone(),
        api_key,
        email: ctx.user.email.clone(),
        name: ctx.user.name.clone(),
    }))
}

pub async fn connect_scope(Auth(ctx): Auth, Json(_body): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "success": true,
        "containerTags": ctx.restricted_container_tags,
        "permission": ctx.permission,
    })))
}

// ---------------- discovery ----------------

pub async fn as_metadata(headers: HeaderMap) -> ApiResult<Json<Value>> {
    let base = base_url(&headers)?;
    Ok(Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/api/auth/oauth2/authorize"),
        "token_endpoint": format!("{base}/api/auth/oauth2/token"),
        "registration_endpoint": format!("{base}/api/auth/oauth2/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "client_secret_post"],
        // Opaque OAuth2 tokens only — no OIDC ID tokens are issued, so `openid` (and the
        // profile/email OIDC claim scopes) must not be advertised.
        "scopes_supported": ["offline_access"],
    })))
}
