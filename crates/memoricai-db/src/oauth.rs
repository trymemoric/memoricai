//! OAuth2 provider storage: clients, authorization codes, access/refresh tokens.

use crate::{db_err, Db};
use chrono::{DateTime, Utc};
use memoricai_core::error::Result;
use sqlx::postgres::PgRow;
use sqlx::Row;

#[derive(Debug, Clone)]
pub struct OAuthClient {
    pub id: String,
    pub client_secret: Option<String>,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub first_party: bool,
}

#[derive(Debug, Clone)]
pub struct OAuthCode {
    pub code: String,
    pub client_id: String,
    pub user_id: String,
    pub org_id: String,
    pub redirect_uri: String,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub scope: Option<String>,
    pub container_tags: Vec<String>,
    pub permission: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub client_id: String,
    pub user_id: String,
    pub org_id: String,
    pub container_tags: Vec<String>,
    pub scope: Option<String>,
    pub permission: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

fn map_client(r: &PgRow) -> OAuthClient {
    OAuthClient {
        id: r.get("id"),
        client_secret: r.get("client_secret"),
        name: r.get("name"),
        redirect_uris: r.get("redirect_uris"),
        grant_types: r.get("grant_types"),
        first_party: r.get("first_party"),
    }
}

fn map_token(r: &PgRow) -> OAuthToken {
    OAuthToken {
        access_token: r.get("access_token"),
        refresh_token: r.get("refresh_token"),
        client_id: r.get("client_id"),
        user_id: r.get("user_id"),
        org_id: r.get("org_id"),
        container_tags: r.get("container_tags"),
        scope: r.get("scope"),
        permission: r.get("permission"),
        access_expires_at: r.get("access_expires_at"),
        refresh_expires_at: r.get("refresh_expires_at"),
        revoked: r.get("revoked"),
    }
}

fn map_code(r: &PgRow) -> OAuthCode {
    OAuthCode {
        code: r.get("code"),
        client_id: r.get("client_id"),
        user_id: r.get("user_id"),
        org_id: r.get("org_id"),
        redirect_uri: r.get("redirect_uri"),
        code_challenge: r.get("code_challenge"),
        code_challenge_method: r.get("code_challenge_method"),
        scope: r.get("scope"),
        container_tags: r.get("container_tags"),
        permission: r.get("permission"),
        expires_at: r.get("expires_at"),
    }
}

impl Db {
    pub async fn insert_oauth_client(&self, c: &OAuthClient) -> Result<()> {
        sqlx::query(
            "INSERT INTO oauth_clients (id, client_secret, name, redirect_uris, grant_types, first_party)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(&c.id)
        .bind(c.client_secret.as_deref().map(crate::crypto::hash_token))
        .bind(&c.name)
        .bind(&c.redirect_uris)
        .bind(&c.grant_types)
        .bind(c.first_party)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Count dynamically-registered (non-first-party) OAuth clients, to cap unbounded
    /// growth from the public registration endpoint.
    pub async fn count_dynamic_oauth_clients(&self) -> Result<i64> {
        let c: i64 = sqlx::query("SELECT count(*) AS c FROM oauth_clients WHERE NOT first_party")
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?
            .get("c");
        Ok(c)
    }

    pub async fn get_oauth_client(&self, id: &str) -> Result<Option<OAuthClient>> {
        let row = sqlx::query("SELECT * FROM oauth_clients WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_client))
    }

    pub async fn insert_oauth_code(&self, c: &OAuthCode) -> Result<()> {
        sqlx::query(
            "INSERT INTO oauth_codes
               (code, client_id, user_id, org_id, redirect_uri, code_challenge,
                code_challenge_method, scope, container_tags, permission, expires_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(&c.code)
        .bind(&c.client_id)
        .bind(&c.user_id)
        .bind(&c.org_id)
        .bind(&c.redirect_uri)
        .bind(&c.code_challenge)
        .bind(&c.code_challenge_method)
        .bind(&c.scope)
        .bind(&c.container_tags)
        .bind(&c.permission)
        .bind(c.expires_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn get_oauth_code(&self, code: &str) -> Result<Option<OAuthCode>> {
        let row = sqlx::query("SELECT * FROM oauth_codes WHERE code = $1")
            .bind(code)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_code))
    }

    /// Atomically consume a code only for the client it was issued to.
    pub async fn take_oauth_code(&self, code: &str, client_id: &str) -> Result<Option<OAuthCode>> {
        let row =
            sqlx::query("DELETE FROM oauth_codes WHERE code = $1 AND client_id = $2 RETURNING *")
                .bind(code)
                .bind(client_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(row.as_ref().map(map_code))
    }

    pub async fn insert_oauth_token(&self, t: &OAuthToken) -> Result<()> {
        sqlx::query(
            "INSERT INTO oauth_tokens
               (access_token, refresh_token, client_id, user_id, org_id, container_tags,
                scope, permission, access_expires_at, refresh_expires_at, revoked)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(crate::crypto::hash_token(&t.access_token))
        .bind(t.refresh_token.as_deref().map(crate::crypto::hash_token))
        .bind(&t.client_id)
        .bind(&t.user_id)
        .bind(&t.org_id)
        .bind(&t.container_tags)
        .bind(&t.scope)
        .bind(&t.permission)
        .bind(t.access_expires_at)
        .bind(t.refresh_expires_at)
        .bind(t.revoked)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn get_oauth_token(&self, access_token: &str) -> Result<Option<OAuthToken>> {
        let row = sqlx::query("SELECT * FROM oauth_tokens WHERE access_token = $1 AND NOT revoked")
            .bind(crate::crypto::hash_token(access_token))
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_token))
    }

    /// Atomically rotate a live refresh token belonging to the authenticated client.
    pub async fn take_oauth_token_by_refresh(
        &self,
        refresh: &str,
        client_id: &str,
    ) -> Result<Option<OAuthToken>> {
        let row = sqlx::query(
            "UPDATE oauth_tokens SET revoked = true
             WHERE refresh_token = $1 AND client_id = $2 AND NOT revoked
             RETURNING *",
        )
        .bind(crate::crypto::hash_token(refresh))
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.as_ref().map(map_token))
    }
}
