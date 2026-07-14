//! Identity + API-key + analytics repository.

use crate::{db_err, Db};
use memoricai_core::enums::OrgRole;
use memoricai_core::error::Result;
use memoricai_core::model::{ApiKeyRecord, Membership, Organization, User};
use sqlx::postgres::PgRow;
use sqlx::Row;

#[derive(Debug, Clone)]
pub struct ApiRequestRecord {
    pub request_type: String,
    pub org_id: Option<String>,
    pub user_id: Option<String>,
    pub key_id: Option<String>,
    pub status_code: i32,
    pub duration_ms: i64,
}

fn map_user(row: &PgRow) -> User {
    User {
        id: row.get("id"),
        email: row.get("email"),
        name: row.get("name"),
    }
}

fn map_org(row: &PgRow) -> Organization {
    Organization {
        id: row.get("id"),
        name: row.get("name"),
        metadata: row.get("metadata"),
    }
}

fn map_api_key(row: &PgRow) -> ApiKeyRecord {
    ApiKeyRecord {
        id: row.get("id"),
        key_hash: row.get("key_hash"),
        prefix: row.get("prefix"),
        last4: row.get("last4"),
        org_id: row.get("org_id"),
        user_id: row.get("user_id"),
        name: row.get("name"),
        key_type: row.get("key_type"),
        container_tag: row.get("container_tag"),
        allowed_endpoints: row.get("allowed_endpoints"),
        rate_limit_max: row.get("rate_limit_max"),
        rate_limit_window_ms: row.get("rate_limit_window_ms"),
        expires_at: row.get("expires_at"),
        revoked: row.get("revoked"),
        created_at: row.get("created_at"),
    }
}

impl Db {
    // ---------- users / orgs / members ----------

    pub async fn insert_user(&self, user: &User) -> Result<()> {
        sqlx::query(
            "INSERT INTO users (id, email, name) VALUES ($1,$2,$3)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&user.id)
        .bind(&user.email)
        .bind(&user.name)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn get_user(&self, id: &str) -> Result<Option<User>> {
        let row = sqlx::query("SELECT * FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_user))
    }

    pub async fn get_user_by_email(&self, email: &str) -> Result<Option<User>> {
        let row = sqlx::query("SELECT * FROM users WHERE email = $1")
            .bind(email)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_user))
    }

    pub async fn insert_org(&self, org: &Organization) -> Result<()> {
        sqlx::query(
            "INSERT INTO organizations (id, name, metadata) VALUES ($1,$2,$3)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&org.id)
        .bind(&org.name)
        .bind(&org.metadata)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn get_org(&self, id: &str) -> Result<Option<Organization>> {
        let row = sqlx::query("SELECT * FROM organizations WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(map_org))
    }

    pub async fn upsert_membership(&self, m: &Membership) -> Result<()> {
        sqlx::query(
            "INSERT INTO members (user_id, org_id, role, access_type, container_tags)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (user_id, org_id) DO UPDATE SET
               role = EXCLUDED.role, access_type = EXCLUDED.access_type,
               container_tags = EXCLUDED.container_tags",
        )
        .bind(&m.user_id)
        .bind(&m.org_id)
        .bind(m.role.as_str())
        .bind(&m.access_type)
        .bind(&m.container_tags)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn get_membership(&self, user_id: &str, org_id: &str) -> Result<Option<Membership>> {
        let row = sqlx::query("SELECT * FROM members WHERE user_id = $1 AND org_id = $2")
            .bind(user_id)
            .bind(org_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.as_ref().map(|r| Membership {
            user_id: r.get("user_id"),
            org_id: r.get("org_id"),
            role: OrgRole::parse(&r.get::<String, _>("role")),
            access_type: r.get("access_type"),
            container_tags: r.get("container_tags"),
        }))
    }

    // ---------- api keys ----------

    pub async fn insert_api_key(&self, k: &ApiKeyRecord) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO api_keys
               (id, key_hash, prefix, last4, org_id, user_id, name, key_type, container_tag,
                allowed_endpoints, rate_limit_max, rate_limit_window_ms, expires_at, revoked, created_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)"#,
        )
        .bind(&k.id)
        .bind(&k.key_hash)
        .bind(&k.prefix)
        .bind(&k.last4)
        .bind(&k.org_id)
        .bind(&k.user_id)
        .bind(&k.name)
        .bind(&k.key_type)
        .bind(&k.container_tag)
        .bind(&k.allowed_endpoints)
        .bind(k.rate_limit_max)
        .bind(k.rate_limit_window_ms)
        .bind(k.expires_at)
        .bind(k.revoked)
        .bind(k.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Non-revoked keys sharing the indexed public prefix and suffix, followed by hash verification.
    pub async fn find_keys_by_prefix_and_last4(
        &self,
        prefix: &str,
        last4: &str,
    ) -> Result<Vec<ApiKeyRecord>> {
        let rows =
            sqlx::query("SELECT * FROM api_keys WHERE prefix = $1 AND last4 = $2 AND NOT revoked")
                .bind(prefix)
                .bind(last4)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(rows.iter().map(map_api_key).collect())
    }

    pub async fn revoke_key(&self, org_id: &str, id: &str) -> Result<bool> {
        // Full org/root keys (`key_type = 'org'`) are intentionally not revocable via
        // the API; scoped keys and MCP session keys are.
        let r = sqlx::query(
            "UPDATE api_keys SET revoked = true
             WHERE org_id = $1 AND id = $2 AND key_type IN ('scoped', 'session')",
        )
        .bind(org_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn count_api_keys(&self) -> Result<i64> {
        let c: i64 = sqlx::query("SELECT count(*) AS c FROM api_keys")
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?
            .get("c");
        Ok(c)
    }

    // ---------- analytics ----------

    pub async fn log_request(
        &self,
        req_type: &str,
        org_id: Option<&str>,
        user_id: Option<&str>,
        key_id: Option<&str>,
        status_code: i32,
        duration_ms: i64,
    ) -> Result<()> {
        self.log_requests_batch(&[ApiRequestRecord {
            request_type: req_type.to_string(),
            org_id: org_id.map(str::to_string),
            user_id: user_id.map(str::to_string),
            key_id: key_id.map(str::to_string),
            status_code,
            duration_ms,
        }])
        .await
    }

    pub async fn log_requests_batch(&self, records: &[ApiRequestRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = records
            .iter()
            .map(|_| memoricai_core::ids::request_id())
            .collect();
        let request_types: Vec<&str> = records
            .iter()
            .map(|record| record.request_type.as_str())
            .collect();
        let org_ids: Vec<Option<&str>> = records
            .iter()
            .map(|record| record.org_id.as_deref())
            .collect();
        let user_ids: Vec<Option<&str>> = records
            .iter()
            .map(|record| record.user_id.as_deref())
            .collect();
        let key_ids: Vec<Option<&str>> = records
            .iter()
            .map(|record| record.key_id.as_deref())
            .collect();
        let status_codes: Vec<i32> = records.iter().map(|record| record.status_code).collect();
        let durations: Vec<i64> = records.iter().map(|record| record.duration_ms).collect();
        sqlx::query(
            "INSERT INTO api_requests
                (id, type, org_id, user_id, key_id, status_code, duration)
             SELECT * FROM unnest(
                $1::text[], $2::text[], $3::text[], $4::text[],
                $5::text[], $6::int4[], $7::int8[])",
        )
        .bind(&ids)
        .bind(&request_types)
        .bind(&org_ids)
        .bind(&user_ids)
        .bind(&key_ids)
        .bind(&status_codes)
        .bind(&durations)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }
}
