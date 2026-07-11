//! memoricai-auth: API-key minting/introspection, container-scoped keys, tenant
//! policy, and a simple fixed-window rate limiter. Keys are hashed at rest
//! (argon2) with an O(1) prefix lookup, then hash-verified.

pub mod oauth;

use std::collections::HashMap;
use std::sync::Mutex;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::Utc;
use memoricai_core::enums::OrgRole;
use memoricai_core::error::{Error, Result};
use memoricai_core::model::{ApiKeyRecord, AuthContext, Membership, Organization, User};
use memoricai_db::Db;

pub struct AuthService {
    db: Db,
    limiter: Mutex<HashMap<String, (i64, u32)>>, // key_id -> (window_start_ms, count)
}

impl AuthService {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            limiter: Mutex::new(HashMap::new()),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Create an org + owner user + a full org API key. Returns the plaintext key (shown once).
    pub async fn bootstrap_org(
        &self,
        org_name: &str,
        email: &str,
    ) -> Result<(Organization, User, String)> {
        let org = Organization {
            id: memoricai_core::ids::org_id(),
            name: org_name.to_string(),
            metadata: serde_json::json!({}),
        };
        // Reuse an existing user with this email, else create one (email is unique).
        let user = match self.db.get_user_by_email(email).await? {
            Some(u) => u,
            None => {
                let u = User {
                    id: memoricai_core::ids::user_id(),
                    email: email.to_string(),
                    name: None,
                };
                self.db.insert_user(&u).await?;
                u
            }
        };
        self.db.insert_org(&org).await?;
        self.db
            .upsert_membership(&Membership {
                user_id: user.id.clone(),
                org_id: org.id.clone(),
                role: memoricai_core::enums::OrgRole::Owner,
                access_type: "full".into(),
                container_tags: vec![],
            })
            .await?;
        let key = self
            .mint_org_key(&org.id, Some(&user.id), "default")
            .await?;
        Ok((org, user, key))
    }

    /// Mint a full org key (`mc_<orgId>_<rand>`), store its hash, return the plaintext.
    pub async fn mint_org_key(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        name: &str,
    ) -> Result<String> {
        let display = memoricai_core::ids::org_api_key(org_id);
        let prefix =
            key_prefix(&display).ok_or_else(|| Error::Internal("bad key format".into()))?;
        let record = ApiKeyRecord {
            id: memoricai_core::ids::api_key_id(),
            key_hash: hash_key(&display)?,
            prefix,
            last4: last4(&display),
            org_id: org_id.to_string(),
            user_id: user_id.map(|s| s.to_string()),
            name: name.to_string(),
            key_type: "org".into(),
            container_tag: None,
            allowed_endpoints: None,
            rate_limit_max: 0, // 0 = unlimited for full org keys
            rate_limit_window_ms: 60_000,
            expires_at: None,
            revoked: false,
            created_at: Utc::now(),
        };
        self.db.insert_api_key(&record).await?;
        Ok(display)
    }

    /// Mint a container-scoped key. Requires a full org key context.
    pub async fn mint_scoped_key(
        &self,
        ctx: &AuthContext,
        container_tag: &str,
        name: Option<&str>,
        expires_in_days: Option<i64>,
        rate_limit_max: i32,
        rate_limit_window_ms: i64,
    ) -> Result<(String, ApiKeyRecord)> {
        if ctx.key_type != "org" && ctx.key_type != "oauth" {
            return Err(Error::Forbidden(
                "only organization or OAuth credentials may mint scoped keys".into(),
            ));
        }
        self.authorize_write(ctx)?;
        self.authorize_container(ctx, container_tag)?;
        if !memoricai_core::is_valid_container_tag(container_tag) {
            return Err(Error::BadRequest("invalid container tag".into()));
        }
        if name.is_some_and(|name| name.trim().is_empty() || name.len() > 100) {
            return Err(Error::BadRequest(
                "scoped key name must contain 1..=100 bytes".into(),
            ));
        }
        if expires_in_days.is_some_and(|days| !(1..=3650).contains(&days)) {
            return Err(Error::BadRequest(
                "expiresInDays must be between 1 and 3650".into(),
            ));
        }
        if !(1..=100_000).contains(&rate_limit_max) {
            return Err(Error::BadRequest(
                "rateLimitMax must be between 1 and 100000".into(),
            ));
        }
        if !(1_000..=86_400_000).contains(&rate_limit_window_ms) {
            return Err(Error::BadRequest(
                "rateLimitTimeWindow must be between 1000 and 86400000 milliseconds".into(),
            ));
        }
        let display = memoricai_core::ids::org_api_key(&ctx.org.id);
        let prefix =
            key_prefix(&display).ok_or_else(|| Error::Internal("bad key format".into()))?;
        let allowed: Vec<String> = memoricai_core::SCOPED_KEY_ALLOWED_ENDPOINTS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let expires_at = expires_in_days.map(|d| Utc::now() + chrono::Duration::days(d));
        let key_name = name
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("scoped_{container_tag}"));
        let record = ApiKeyRecord {
            id: memoricai_core::ids::api_key_id(),
            key_hash: hash_key(&display)?,
            prefix,
            last4: last4(&display),
            org_id: ctx.org.id.clone(),
            user_id: Some(ctx.user.id.clone()),
            name: key_name,
            key_type: "scoped".into(),
            container_tag: Some(container_tag.to_string()),
            allowed_endpoints: Some(allowed),
            rate_limit_max,
            rate_limit_window_ms,
            expires_at,
            revoked: false,
            created_at: Utc::now(),
        };
        self.db.insert_api_key(&record).await?;
        Ok((display, record))
    }

    /// Validate a bearer token and resolve the request's identity + scope.
    pub async fn introspect(&self, token: &str) -> Result<AuthContext> {
        if !token.starts_with("mc_") {
            return Err(Error::Unauthorized("expected mc_ bearer token".into()));
        }
        let prefix =
            key_prefix(token).ok_or_else(|| Error::Unauthorized("malformed key".into()))?;
        let token_last4 = last4(token);
        let candidates = self
            .db
            .find_keys_by_prefix_and_last4(&prefix, &token_last4)
            .await?;
        let now = Utc::now();
        for rec in candidates {
            if let Some(exp) = rec.expires_at {
                if exp < now {
                    continue;
                }
            }
            let tok = token.to_owned();
            let hash = rec.key_hash.clone();
            let verified = tokio::task::spawn_blocking(move || verify_key(&tok, &hash))
                .await
                .unwrap_or(false);
            if verified {
                self.enforce_rate_limit(&rec)?;
                return self.build_context(rec).await;
            }
        }
        Err(Error::Unauthorized("invalid api key".into()))
    }

    /// Validate any bearer: `mc_` API keys or OAuth2 access tokens.
    pub async fn introspect_bearer(&self, token: &str) -> Result<AuthContext> {
        if token.starts_with("mc_") {
            self.introspect(token).await
        } else {
            self.introspect_oauth(token).await
        }
    }

    /// Validate an OAuth2 access token and resolve identity + scope.
    pub async fn introspect_oauth(&self, token: &str) -> Result<AuthContext> {
        let t = self
            .db
            .get_oauth_token(token)
            .await?
            .ok_or_else(|| Error::Unauthorized("invalid oauth token".into()))?;
        if t.access_expires_at < Utc::now() {
            return Err(Error::Unauthorized("oauth token expired".into()));
        }
        let (org, user, membership) = tokio::try_join!(
            self.db.get_org(&t.org_id),
            self.db.get_user(&t.user_id),
            self.db.get_membership(&t.user_id, &t.org_id)
        )?;
        let org = org.ok_or_else(|| Error::Unauthorized("organization no longer exists".into()))?;
        let user = user.ok_or_else(|| Error::Unauthorized("user no longer exists".into()))?;
        let membership = membership.ok_or_else(|| {
            Error::Unauthorized("organization membership no longer exists".into())
        })?;
        let scoped_container_tag = if t.container_tags.len() == 1 {
            Some(t.container_tags[0].clone())
        } else {
            None
        };
        let member_restriction =
            (membership.access_type == "restricted").then(|| membership.container_tags.clone());
        let token_restriction = (!t.container_tags.is_empty()).then_some(t.container_tags);
        let restricted = intersect_restrictions(token_restriction, member_restriction);
        Ok(AuthContext {
            user,
            org,
            key_id: format!("oauth:{}", t.client_id),
            key_type: "oauth".into(),
            permission: t.permission,
            org_role: Some(membership.role),
            allowed_endpoints: None,
            scoped_container_tag,
            restricted_container_tags: restricted,
        })
    }

    async fn build_context(&self, rec: ApiKeyRecord) -> Result<AuthContext> {
        let user_membership_fut = async {
            match &rec.user_id {
                Some(uid) => {
                    tokio::try_join!(
                        self.db.get_user(uid),
                        self.db.get_membership(uid, &rec.org_id)
                    )
                }
                None => Ok((None, None)),
            }
        };
        let (org, (user, membership)) =
            tokio::try_join!(self.db.get_org(&rec.org_id), user_membership_fut)?;
        let org = org.ok_or_else(|| Error::Unauthorized("organization no longer exists".into()))?;
        let user = match &rec.user_id {
            Some(_) => user.ok_or_else(|| Error::Unauthorized("user no longer exists".into()))?,
            None => User {
                id: "user_system".into(),
                email: "system".into(),
                name: None,
            },
        };
        if rec.user_id.is_some() && membership.is_none() {
            return Err(Error::Unauthorized(
                "organization membership no longer exists".into(),
            ));
        }
        let restricted = match &membership {
            Some(m) if m.access_type == "restricted" => Some(m.container_tags.clone()),
            _ => None,
        };
        Ok(AuthContext {
            user,
            org,
            key_id: rec.id,
            key_type: rec.key_type,
            permission: "write".into(),
            org_role: membership.map(|m| m.role),
            allowed_endpoints: rec.allowed_endpoints,
            scoped_container_tag: rec.container_tag,
            restricted_container_tags: restricted,
        })
    }

    /// Enforce endpoint capability and read/write permission for one HTTP request.
    pub fn authorize_request(
        &self,
        ctx: &AuthContext,
        method: &str,
        endpoint_path: &str,
    ) -> Result<()> {
        self.authorize_endpoint(ctx, endpoint_path)?;
        if ctx.permission == "read" && !is_read_operation(method, endpoint_path) {
            return Err(Error::Forbidden(
                "read-only credential cannot mutate data".into(),
            ));
        }
        Ok(())
    }

    /// Enforce only the endpoint capability. Resource scope is checked after extraction.
    pub fn authorize_endpoint(&self, ctx: &AuthContext, endpoint_path: &str) -> Result<()> {
        if !endpoint_is_allowed(ctx, endpoint_path) {
            return Err(Error::Forbidden(format!(
                "scoped key not permitted on {endpoint_path}"
            )));
        }
        Ok(())
    }

    pub fn authorize_write(&self, ctx: &AuthContext) -> Result<()> {
        if ctx.permission == "read" {
            return Err(Error::Forbidden(
                "read-only credential cannot mutate data".into(),
            ));
        }
        Ok(())
    }

    pub fn authorize_admin(&self, ctx: &AuthContext) -> Result<()> {
        match ctx.org_role {
            Some(OrgRole::Owner | OrgRole::Admin) | None => Ok(()),
            Some(OrgRole::Member) => {
                Err(Error::Forbidden("organization admin role required".into()))
            }
        }
    }

    /// Effective container allowlist. `None` means unrestricted.
    pub fn allowed_container_tags(&self, ctx: &AuthContext) -> Option<Vec<String>> {
        allowed_container_tags_for(ctx)
    }

    pub fn is_container_restricted(&self, ctx: &AuthContext) -> bool {
        self.allowed_container_tags(ctx).is_some()
    }

    /// Validate and resolve a possibly omitted list of tags.
    pub fn scope_tags(
        &self,
        ctx: &AuthContext,
        requested: Option<&[String]>,
    ) -> Result<Option<Vec<String>>> {
        let requested = requested.filter(|tags| !tags.is_empty());
        if let Some(tags) = requested {
            if tags.len() > 20
                || tags
                    .iter()
                    .any(|tag| !memoricai_core::is_valid_container_tag(tag))
                || tags
                    .iter()
                    .enumerate()
                    .any(|(index, tag)| tags[..index].iter().any(|candidate| candidate == tag))
            {
                return Err(Error::BadRequest(
                    "container tags must contain at most 20 unique valid tags".into(),
                ));
            }
        }
        let Some(allowed) = self.allowed_container_tags(ctx) else {
            return Ok(requested.map(|tags| tags.to_vec()));
        };
        if allowed.is_empty() {
            return Err(Error::Forbidden(
                "credential has no permitted containers".into(),
            ));
        }
        match requested {
            Some(tags) => {
                if tags
                    .iter()
                    .all(|tag| allowed.iter().any(|candidate| candidate == tag))
                {
                    Ok(Some(tags.to_vec()))
                } else {
                    Err(Error::Forbidden(
                        "container tag outside credential scope".into(),
                    ))
                }
            }
            None => Ok(Some(allowed)),
        }
    }

    /// Validate and resolve one optional tag. A single allowed tag is the secure default.
    pub fn scope_tag(&self, ctx: &AuthContext, requested: Option<&str>) -> Result<Option<String>> {
        if let Some(tag) = requested {
            self.authorize_container(ctx, tag)?;
            return Ok(Some(tag.to_string()));
        }
        match self.allowed_container_tags(ctx) {
            None => Ok(None),
            Some(tags) if tags.len() == 1 => Ok(tags.into_iter().next()),
            Some(tags) if tags.is_empty() => Err(Error::Forbidden(
                "credential has no permitted containers".into(),
            )),
            Some(_) => Err(Error::BadRequest(
                "containerTag is required for a multi-container credential".into(),
            )),
        }
    }

    pub fn authorize_container(&self, ctx: &AuthContext, tag: &str) -> Result<()> {
        if !memoricai_core::is_valid_container_tag(tag) {
            return Err(Error::BadRequest("invalid container tag".into()));
        }
        if let Some(allowed) = self.allowed_container_tags(ctx) {
            if !allowed.iter().any(|candidate| candidate == tag) {
                return Err(Error::Forbidden(
                    "container tag outside credential scope".into(),
                ));
            }
        }
        Ok(())
    }

    /// Authorize a resource shared by one or more containers.
    pub fn authorize_resource_tags(&self, ctx: &AuthContext, tags: &[String]) -> Result<()> {
        let Some(allowed) = self.allowed_container_tags(ctx) else {
            return Ok(());
        };
        if tags
            .iter()
            .any(|tag| allowed.iter().any(|candidate| candidate == tag))
        {
            Ok(())
        } else {
            Err(Error::Forbidden(
                "resource is outside credential scope".into(),
            ))
        }
    }

    /// Authorize a mutation of a resource shared by one or more containers.
    /// Restricted credentials must control every container the mutation affects.
    pub fn authorize_resource_write_tags(&self, ctx: &AuthContext, tags: &[String]) -> Result<()> {
        self.authorize_write(ctx)?;
        let Some(allowed) = self.allowed_container_tags(ctx) else {
            return Ok(());
        };
        if resource_tags_are_subset(tags, &allowed) {
            Ok(())
        } else {
            Err(Error::Forbidden(
                "mutation would affect a resource shared with an unauthorized container".into(),
            ))
        }
    }

    /// Backward-compatible combined endpoint + optional-container check.
    pub fn authorize(
        &self,
        ctx: &AuthContext,
        endpoint_path: &str,
        container_tag: Option<&str>,
    ) -> Result<()> {
        self.authorize_endpoint(ctx, endpoint_path)?;
        if let Some(tag) = container_tag {
            self.authorize_container(ctx, tag)?;
        }
        Ok(())
    }

    fn enforce_rate_limit(&self, rec: &ApiKeyRecord) -> Result<()> {
        if rec.rate_limit_max <= 0 {
            return Ok(());
        }
        let now_ms = Utc::now().timestamp_millis();
        let mut map = self.limiter.lock().unwrap();
        let entry = map.entry(rec.id.clone()).or_insert((now_ms, 0));
        if now_ms - entry.0 >= rec.rate_limit_window_ms {
            *entry = (now_ms, 0);
        }
        entry.1 += 1;
        if entry.1 > rec.rate_limit_max as u32 {
            return Err(Error::RateLimited);
        }
        Ok(())
    }
}

fn endpoint_is_allowed(ctx: &AuthContext, endpoint_path: &str) -> bool {
    ctx.allowed_endpoints.as_ref().is_none_or(|allowed| {
        allowed.iter().any(|endpoint| {
            endpoint_path == endpoint || endpoint_path.starts_with(&format!("{endpoint}/"))
        })
    })
}

fn allowed_container_tags_for(ctx: &AuthContext) -> Option<Vec<String>> {
    let scoped = ctx
        .scoped_container_tag
        .as_ref()
        .map(|tag| vec![tag.clone()]);
    intersect_restrictions(scoped, ctx.restricted_container_tags.clone())
}

fn resource_tags_are_subset(tags: &[String], allowed: &[String]) -> bool {
    !tags.is_empty()
        && tags
            .iter()
            .all(|tag| allowed.iter().any(|candidate| candidate == tag))
}

fn intersect_restrictions(
    left: Option<Vec<String>>,
    right: Option<Vec<String>>,
) -> Option<Vec<String>> {
    match (left, right) {
        (None, None) => None,
        (Some(mut tags), None) | (None, Some(mut tags)) => {
            tags.sort();
            tags.dedup();
            Some(tags)
        }
        (Some(left), Some(right)) => {
            let mut tags: Vec<String> = left
                .into_iter()
                .filter(|tag| right.iter().any(|candidate| candidate == tag))
                .collect();
            tags.sort();
            tags.dedup();
            Some(tags)
        }
    }
}

fn is_read_operation(method: &str, path: &str) -> bool {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return true;
    }
    method == "POST"
        && matches!(
            path,
            "/v1/documents/list"
                | "/v1/documents/documents"
                | "/v1/documents/search"
                | "/v1/search"
                | "/v1/profile"
                | "/v1/profile/buckets"
                | "/v1/connections/list"
        )
}

// ---------------- crypto helpers ----------------

fn hash_key(key: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(key.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::Internal(format!("hash: {e}")))
}

fn verify_key(key: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(key.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// `mc_<orgId>_<rand>` -> `mc_<orgId>` lookup prefix.
fn key_prefix(token: &str) -> Option<String> {
    let org = token.strip_prefix("mc_")?.split('_').next()?;
    Some(format!("mc_{org}"))
}

fn last4(s: &str) -> String {
    let suffix: String = s.chars().rev().take(4).collect();
    suffix.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> AuthContext {
        AuthContext {
            user: User {
                id: "user_test".into(),
                email: "test@example.com".into(),
                name: None,
            },
            org: Organization {
                id: "org_test".into(),
                name: "Test".into(),
                metadata: serde_json::json!({}),
            },
            key_id: "key_test".into(),
            key_type: "scoped".into(),
            permission: "write".into(),
            org_role: Some(OrgRole::Member),
            allowed_endpoints: Some(vec!["/v1/documents".into()]),
            scoped_container_tag: Some("project_a".into()),
            restricted_container_tags: Some(vec!["project_a".into(), "project_b".into()]),
        }
    }

    #[test]
    fn restrictions_intersect_instead_of_failing_open() {
        let ctx = context();
        assert_eq!(
            allowed_container_tags_for(&ctx),
            Some(vec!["project_a".into()])
        );

        let mut disjoint = ctx;
        disjoint.restricted_container_tags = Some(vec!["project_b".into()]);
        assert_eq!(allowed_container_tags_for(&disjoint), Some(vec![]));
    }

    #[test]
    fn endpoint_allowlist_matches_path_boundaries() {
        let ctx = context();
        assert!(endpoint_is_allowed(&ctx, "/v1/documents/abc"));
        assert!(!endpoint_is_allowed(&ctx, "/v1/documents-export"));
    }

    #[test]
    fn shared_resource_mutations_require_every_tag() {
        let allowed = vec!["project_a".to_string()];
        assert!(resource_tags_are_subset(&["project_a".into()], &allowed));
        assert!(!resource_tags_are_subset(
            &["project_a".into(), "project_b".into()],
            &allowed
        ));
    }

    #[test]
    fn read_only_post_allowlist_is_semantic_and_exact() {
        assert!(is_read_operation("POST", "/v1/search"));
        assert!(!is_read_operation("POST", "/v1/memories"));
        assert!(!is_read_operation("POST", "/v1/search/export"));
    }

    #[test]
    fn key_suffix_handles_untrusted_unicode_without_panicking() {
        assert_eq!(last4("mc_org_💥token"), "oken");
        assert_eq!(last4("💥"), "💥");
    }
}
