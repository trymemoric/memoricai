//! Generic OAuth2 authorize-URL construction + code/refresh token exchange.
//! Provider client credentials come from env: `MEMORICAI_<PROVIDER>_CLIENT_ID`
//! and `_CLIENT_SECRET` (provider name uppercased, `-` → `_`).

use crate::{http, TokenSet};
use base64::Engine as _;
use chrono::{Duration, Utc};
use memoricai_core::error::{Error, Result};
use serde_json::json;

pub struct OAuthConfig {
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static str,
}

pub fn provider_config(provider: &str) -> Option<OAuthConfig> {
    Some(match provider {
        "google-drive" => OAuthConfig {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scopes: "https://www.googleapis.com/auth/drive.readonly",
        },
        "gmail" => OAuthConfig {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scopes: "https://www.googleapis.com/auth/gmail.readonly",
        },
        "notion" => OAuthConfig {
            auth_url: "https://api.notion.com/v1/oauth/authorize",
            token_url: "https://api.notion.com/v1/oauth/token",
            scopes: "",
        },
        "onedrive" => OAuthConfig {
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            scopes: "Files.Read.All offline_access User.Read",
        },
        "github" => OAuthConfig {
            auth_url: "https://github.com/login/oauth/authorize",
            token_url: "https://github.com/login/oauth/access_token",
            scopes: "repo,user:email,admin:repo_hook",
        },
        _ => return None,
    })
}

fn env_key(provider: &str, suffix: &str) -> String {
    format!(
        "MEMORICAI_{}_{}",
        provider.to_uppercase().replace('-', "_"),
        suffix
    )
}

pub fn client_id(provider: &str) -> Result<String> {
    std::env::var(env_key(provider, "CLIENT_ID")).map_err(|_| {
        Error::BadRequest(format!(
            "missing {} (OAuth client id)",
            env_key(provider, "CLIENT_ID")
        ))
    })
}

pub fn client_secret(provider: &str) -> Result<String> {
    std::env::var(env_key(provider, "CLIENT_SECRET"))
        .map_err(|_| Error::BadRequest(format!("missing {}", env_key(provider, "CLIENT_SECRET"))))
}

pub fn base_url() -> String {
    std::env::var("MEMORICAI_BASE_URL").unwrap_or_else(|_| "http://localhost:7373".to_string())
}

pub fn redirect_uri(provider: &str) -> String {
    format!(
        "{}/v1/connections/auth/callback/{provider}",
        base_url().trim_end_matches('/')
    )
}

pub fn authorize_url(
    cfg: &OAuthConfig,
    _base_url: &str,
    provider: &str,
    state: &str,
    client_id: &str,
) -> String {
    let redirect = redirect_uri(provider);
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}",
        cfg.auth_url,
        urlencode(client_id),
        urlencode(&redirect),
        urlencode(state),
    );
    if !cfg.scopes.is_empty() {
        url.push_str(&format!("&scope={}", urlencode(cfg.scopes)));
    }
    // Google needs offline access + consent to return a refresh token.
    if provider == "google-drive" || provider == "gmail" {
        url.push_str("&access_type=offline&prompt=consent");
    }
    if provider == "notion" {
        url.push_str("&owner=user");
    }
    url
}

pub async fn exchange_code(provider: &str, code: &str) -> Result<TokenSet> {
    let cfg =
        provider_config(provider).ok_or_else(|| Error::BadRequest("no oauth config".into()))?;
    let cid = client_id(provider)?;
    let secret = client_secret(provider)?;
    let redirect = redirect_uri(provider);
    let client = http();

    let mut req = client
        .post(cfg.token_url)
        .header("Accept", "application/json");
    if provider == "notion" {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("{cid}:{secret}"));
        req = req
            .header("Authorization", format!("Basic {basic}"))
            .json(&json!({
                "grant_type": "authorization_code",
                "code": code,
                "redirect_uri": redirect,
            }));
    } else {
        req = req.form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect.as_str()),
            ("client_id", cid.as_str()),
            ("client_secret", secret.as_str()),
        ]);
    }
    let resp = req.send().await.map_err(|e| Error::Model(e.to_string()))?;
    if !resp.status().is_success() {
        let s = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(Error::BadRequest(format!("token exchange {s}: {t}")));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| Error::Model(e.to_string()))?;
    parse_token(&v)
}

pub async fn refresh(provider: &str, refresh_token: &str) -> Result<TokenSet> {
    let cfg =
        provider_config(provider).ok_or_else(|| Error::BadRequest("no oauth config".into()))?;
    let cid = client_id(provider)?;
    let secret = client_secret(provider)?;
    let resp = http()
        .post(cfg.token_url)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", cid.as_str()),
            ("client_secret", secret.as_str()),
        ])
        .send()
        .await
        .map_err(|e| Error::Model(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(Error::Unauthorized(format!(
            "refresh failed: {}",
            resp.status()
        )));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| Error::Model(e.to_string()))?;
    parse_token(&v)
}

fn parse_token(v: &serde_json::Value) -> Result<TokenSet> {
    let access = v["access_token"]
        .as_str()
        .ok_or_else(|| Error::BadRequest("no access_token in response".into()))?
        .to_string();
    let refresh = v["refresh_token"].as_str().map(|s| s.to_string());
    let expires_at = v["expires_in"]
        .as_i64()
        .map(|secs| Utc::now() + Duration::seconds(secs));
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
        email: None,
    })
}

/// Strict RFC-3986 percent-encoding (unreserved set exactly `A-Za-z0-9-_.~`, uppercase percent
/// hex). Also load-bearing for AWS SigV4 canonical URI/query encoding in the S3 connector —
/// do not loosen the escaping rules.
pub fn urlencode(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
