//! Connector storage: connections, credentials, OAuth CSRF state, and sync runs.

use crate::{db_err, Db};
use chrono::{DateTime, Utc};
use memoricai_core::error::{Error, Result};
use memoricai_core::model::{Connection, ConnectionCredentials, SyncRun};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::Row;

fn map_connection(r: &PgRow) -> Connection {
    Connection {
        id: r.get("id"),
        provider: r.get("provider"),
        org_id: r.get("org_id"),
        user_id: r.get("user_id"),
        email: r.get("email"),
        document_limit: r.get("document_limit"),
        container_tags: r
            .get::<Option<Vec<String>>, _>("container_tags")
            .unwrap_or_default(),
        expires_at: r.get("expires_at"),
        metadata: r.get("metadata"),
        last_synced_at: r.get("last_synced_at"),
        created_at: r.get("created_at"),
    }
}

fn map_sync_run(r: &PgRow) -> SyncRun {
    SyncRun {
        id: r.get("id"),
        connection_id: r.get("connection_id"),
        status: r.get("status"),
        trigger_type: r.get("trigger_type"),
        error_kind: r.get("error_kind"),
        started_at: r.get("started_at"),
        completed_at: r.get("completed_at"),
        items_processed: r.get("items_processed"),
        items_failed: r.get("items_failed"),
        error: r.get("error"),
    }
}

/// CSRF state persisted during a connector OAuth flow.
pub struct ConnState {
    pub state_token: String,
    pub provider: String,
    pub org_id: String,
    pub user_id: Option<String>,
    pub redirect_url: Option<String>,
    pub container_tags: Vec<String>,
    pub document_limit: i32,
    pub metadata: Value,
    pub expires_at: DateTime<Utc>,
}

impl Db {
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_connection(
        &self,
        id: &str,
        provider: &str,
        org_id: &str,
        user_id: Option<&str>,
        container_tags: &[String],
        document_limit: i32,
        metadata: &Value,
    ) -> Result<()> {
        // Encrypt sensitive metadata fields (e.g. S3 secretAccessKey, Granola apiKey) at rest.
        let mut metadata = metadata.clone();
        crate::crypto::encrypt_metadata(&mut metadata)?;
        sqlx::query(
            "INSERT INTO connections (id, provider, org_id, user_id, document_limit, container_tags, metadata)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(id)
        .bind(provider)
        .bind(org_id)
        .bind(user_id)
        .bind(document_limit)
        .bind(container_tags)
        .bind(&metadata)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn get_connection(&self, org_id: &str, id: &str) -> Result<Option<Connection>> {
        let row = sqlx::query("SELECT * FROM connections WHERE org_id = $1 AND id = $2")
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        let Some(row) = row.as_ref() else {
            return Ok(None);
        };
        let mut connection = map_connection(row);
        crate::crypto::decrypt_metadata(&mut connection.metadata)?;
        Ok(Some(connection))
    }

    pub async fn get_connection_credentials(
        &self,
        id: &str,
    ) -> Result<Option<ConnectionCredentials>> {
        let row = sqlx::query(
            "SELECT access_token, refresh_token, expires_at, sync_cursor FROM connections WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        let Some(row) = row.as_ref() else {
            return Ok(None);
        };
        Ok(Some(ConnectionCredentials {
            access_token: crate::crypto::decrypt_opt(row.get("access_token"))?,
            refresh_token: crate::crypto::decrypt_opt(row.get("refresh_token"))?,
            expires_at: row.get("expires_at"),
            sync_cursor: crate::crypto::decrypt_opt(row.get("sync_cursor"))?,
        }))
    }

    pub async fn list_connections(
        &self,
        org_id: &str,
        provider: Option<&str>,
        container_tags: Option<&[String]>,
    ) -> Result<Vec<Connection>> {
        let rows = sqlx::query(
            "SELECT * FROM connections WHERE org_id = $1
             AND ($2::text IS NULL OR provider = $2)
             AND ($3::text[] IS NULL OR container_tags && $3)
             ORDER BY created_at DESC",
        )
        .bind(org_id)
        .bind(provider)
        .bind(container_tags)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_connection).collect())
    }

    pub async fn delete_connection(
        &self,
        org_id: &str,
        id: &str,
        delete_documents: bool,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        if delete_documents {
            let document_ids: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM documents WHERE org_id=$1 AND connection_id=$2 FOR UPDATE",
            )
            .bind(org_id)
            .bind(id)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_err)?;
            crate::memories::prepare_memories_for_document_deletion(&mut tx, &document_ids).await?;
            sqlx::query("DELETE FROM documents WHERE id=ANY($1)")
                .bind(&document_ids)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }
        let r = sqlx::query("DELETE FROM connections WHERE org_id = $1 AND id = $2")
            .bind(org_id)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn update_connection_tokens(
        &self,
        id: &str,
        access: Option<&str>,
        refresh: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
        email: Option<&str>,
    ) -> Result<()> {
        let access = crate::crypto::encrypt_opt(access)?;
        let refresh = crate::crypto::encrypt_opt(refresh)?;
        sqlx::query(
            "UPDATE connections SET access_token = COALESCE($2, access_token),
             refresh_token = COALESCE($3, refresh_token),
             expires_at = COALESCE($4, expires_at),
             email = COALESCE($5, email) WHERE id = $1",
        )
        .bind(id)
        .bind(access.as_deref())
        .bind(refresh.as_deref())
        .bind(expires_at)
        .bind(email)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn set_connection_synced(&self, id: &str, cursor: Option<&str>) -> Result<()> {
        // Provider delta links/history cursors can be bearer-like capabilities too.
        let cursor = crate::crypto::encrypt_opt(cursor)?;
        sqlx::query(
            "UPDATE connections SET last_synced_at = now(), sync_cursor = COALESCE($2, sync_cursor) WHERE id = $1",
        )
        .bind(id)
        .bind(cursor.as_deref())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Encrypt legacy plaintext connector credentials and sensitive metadata in place.
    /// Safe to run on every startup: the `enc:v1:` envelope is idempotently preserved.
    pub async fn migrate_connection_credentials(&self) -> Result<u64> {
        if !crate::crypto::encryption_configured()? {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let rows = sqlx::query(
            "SELECT id, access_token, refresh_token, sync_cursor, metadata
             FROM connections FOR UPDATE",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        let mut migrated = 0u64;
        for row in rows {
            let access: Option<String> = row.get("access_token");
            let refresh: Option<String> = row.get("refresh_token");
            let cursor: Option<String> = row.get("sync_cursor");
            let mut metadata: Value = row.get("metadata");
            let needs_migration = access
                .as_deref()
                .is_some_and(|v| !crate::crypto::is_encrypted(v))
                || refresh
                    .as_deref()
                    .is_some_and(|v| !crate::crypto::is_encrypted(v))
                || cursor
                    .as_deref()
                    .is_some_and(|v| !crate::crypto::is_encrypted(v));
            let before_metadata = metadata.clone();
            crate::crypto::encrypt_metadata(&mut metadata)?;
            if needs_migration || metadata != before_metadata {
                sqlx::query(
                    "UPDATE connections SET access_token=$2, refresh_token=$3,
                            sync_cursor=$4, metadata=$5 WHERE id=$1",
                )
                .bind(row.get::<String, _>("id"))
                .bind(crate::crypto::encrypt_opt(access.as_deref())?)
                .bind(crate::crypto::encrypt_opt(refresh.as_deref())?)
                .bind(crate::crypto::encrypt_opt(cursor.as_deref())?)
                .bind(metadata)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
                migrated += 1;
            }
        }
        tx.commit().await.map_err(db_err)?;
        Ok(migrated)
    }

    /// Connections whose last sync is older than `hours` (for the cron sweep).
    pub async fn connections_due(&self, hours: i64) -> Result<Vec<Connection>> {
        let rows = sqlx::query(
            "SELECT * FROM connections
             WHERE last_synced_at IS NULL OR last_synced_at < now() - ($1 || ' hours')::interval",
        )
        .bind(hours.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_connection).collect())
    }

    // ---------- oauth CSRF state ----------

    pub async fn insert_connection_state(&self, s: &ConnState) -> Result<()> {
        sqlx::query(
            "INSERT INTO connection_state
               (state_token, provider, org_id, user_id, redirect_url, container_tags, document_limit, metadata, expires_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(&s.state_token)
        .bind(&s.provider)
        .bind(&s.org_id)
        .bind(&s.user_id)
        .bind(&s.redirect_url)
        .bind(&s.container_tags)
        .bind(s.document_limit)
        .bind(&s.metadata)
        .bind(s.expires_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn take_connection_state(&self, token: &str) -> Result<Option<ConnState>> {
        // Only consume a non-expired state; expired states must not authorize a callback.
        // Opportunistically purge accumulated expired rows in the same round-trip.
        let _ = sqlx::query("DELETE FROM connection_state WHERE expires_at <= now()")
            .execute(&self.pool)
            .await;
        let row = sqlx::query(
            "DELETE FROM connection_state WHERE state_token = $1 AND expires_at > now() RETURNING *",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.as_ref().map(|r| ConnState {
            state_token: r.get("state_token"),
            provider: r.get("provider"),
            org_id: r.get("org_id"),
            user_id: r.get("user_id"),
            redirect_url: r.get("redirect_url"),
            container_tags: r.get("container_tags"),
            document_limit: r.get("document_limit"),
            metadata: r.get("metadata"),
            expires_at: r.get("expires_at"),
        }))
    }

    // ---------- sync runs ----------

    pub async fn start_sync_run(&self, connection_id: &str, trigger_type: &str) -> Result<String> {
        let id = memoricai_core::ids::sync_run_id();
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query(
            "UPDATE sync_runs SET status='failed', completed_at=now(), error_kind='abandoned',
                    error='sync worker lease expired', lease_until=NULL
             WHERE connection_id=$1 AND status='running' AND lease_until < now()",
        )
        .bind(connection_id)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        let inserted = sqlx::query(
            "INSERT INTO sync_runs (id, connection_id, status, trigger_type, lease_until)
             VALUES ($1,$2,'running',$3,now()+interval '5 minutes')
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(&id)
        .bind(connection_id)
        .bind(trigger_type)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        inserted
            .map(|_| id)
            .ok_or_else(|| Error::Conflict("connection sync is already running".into()))
    }

    pub async fn renew_sync_run(&self, id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE sync_runs SET lease_until=now()+interval '5 minutes'
             WHERE id=$1 AND status='running'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn finish_sync_run(
        &self,
        id: &str,
        status: &str,
        processed: i32,
        failed: i32,
        error: Option<&str>,
        error_kind: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE sync_runs SET status = $2, completed_at = now(), lease_until=NULL,
             items_processed = $3, items_failed = $4, error = $5, error_kind = $6 WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(processed)
        .bind(failed)
        .bind(error)
        .bind(error_kind)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    pub async fn list_sync_runs(&self, connection_id: &str) -> Result<Vec<SyncRun>> {
        let rows = sqlx::query(
            "SELECT * FROM sync_runs WHERE connection_id = $1 ORDER BY started_at DESC LIMIT 50",
        )
        .bind(connection_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.iter().map(map_sync_run).collect())
    }
}
