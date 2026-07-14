//! memoricai-db: sqlx repositories over Postgres. pgvector is used through raw
//! SQL casts (`$n::vector`, `<=>`) so no pgvector crate dependency is needed and
//! the embedding dimension stays configurable. Only runtime queries (no
//! compile-time `query!` macros) so building never needs a live database.

pub mod analytics;
pub mod auth;
pub mod buckets;
pub mod connections;
pub mod crypto;
pub mod documents;
pub mod embeddings;
pub mod memories;
pub mod oauth;
pub mod settings;
pub mod spaces;

use memoricai_core::enums::{DocumentStatus, MemoryRelation};
use memoricai_core::error::{Error, Result};
use memoricai_core::model::{Document, Memory};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{Executor, PgPool, Row};

/// Canonical schema for a fresh installation. Existing schemas are deliberately
/// not upgraded: this release requires recreating the database. `include_str!`
/// keeps the binary self-contained without enabling sqlx's macros feature.
const SCHEMA_VERSION: &str = "schema_v1";
const SCHEMA_SQL: &str = include_str!("../migrations/0001_init.sql");

#[derive(Clone)]
pub struct Db {
    pub pool: PgPool,
}

impl Db {
    pub async fn connect(url: &str) -> Result<Self> {
        // A single search fans out several concurrent queries (buffered) and the server
        // allows many inflight requests, so a fixed pool of 10 starves under modest load.
        // Default higher, allow tuning, and cap acquisition wait so starvation surfaces as
        // a fast, bounded error instead of an unbounded stall.
        let max_connections = std::env::var("MEMORICAI_DB_MAX_CONNECTIONS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(20);
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(url)
            .await
            .map_err(db_err)?;
        Ok(Self { pool })
    }

    /// Install the canonical schema once under a database advisory lock.
    pub async fn migrate(&self) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        // Serialize startup migrations across replicas. The transaction-scoped
        // lock is automatically released on commit, rollback, or disconnect.
        tx.execute("SELECT pg_advisory_xact_lock(hashtext('memoricai_schema_migrations'))")
            .await
            .map_err(db_err)?;
        tx.execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
                version TEXT PRIMARY KEY, \
                applied_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        )
        .await
        .map_err(db_err)?;

        let installed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM schema_migrations WHERE version = $1)",
        )
        .bind(SCHEMA_VERSION)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        if !installed {
            // The schema contains multiple statements, so execute it through the
            // simple-query protocol as one transactional batch.
            tx.execute(SCHEMA_SQL)
                .await
                .map_err(|e| Error::Database(format!("installing schema: {e}")))?;
            sqlx::query("INSERT INTO schema_migrations (version) VALUES ($1)")
                .bind(SCHEMA_VERSION)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        // HNSW indexes must be created outside the migration transaction because
        // CREATE INDEX CONCURRENTLY is illegal inside a transaction block.
        self.ensure_all_ann_indexes().await?;
        Ok(())
    }
}

pub(crate) fn db_err(e: sqlx::Error) -> Error {
    match e {
        sqlx::Error::RowNotFound => Error::NotFound("row not found".into()),
        other => Error::Database(other.to_string()),
    }
}

/// Run a count query (must alias its count column `AS c`) and a rows query concurrently.
pub(crate) async fn count_and_rows<'q>(
    pool: &PgPool,
    count_q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    rows_q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
) -> Result<(i64, Vec<PgRow>)> {
    let (count_row, rows) =
        tokio::try_join!(count_q.fetch_one(pool), rows_q.fetch_all(pool)).map_err(db_err)?;
    Ok((count_row.get("c"), rows))
}

/// Format a vector as a pgvector literal (`[1,2,3]`) for `$n::vector` binding.
pub fn pgvec(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    use std::fmt::Write as _;
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "{x}");
    }
    s.push(']');
    s
}

/// A memory returned from vector search, with its similarity score.
pub struct MemoryHit {
    pub memory: Memory,
    pub similarity: f32,
}

/// A chunk returned from vector search, carrying its source document's metadata
/// so callers can apply metadata filters to document/chunk results.
pub struct ChunkScore {
    pub document_id: String,
    pub content: String,
    pub doc_metadata: serde_json::Value,
    pub similarity: f32,
}

// ---------------- row mappers ----------------

pub(crate) fn map_document(row: &PgRow) -> Document {
    Document {
        id: row.get("id"),
        custom_id: row.get("custom_id"),
        content_hash: row.get("content_hash"),
        org_id: row.get("org_id"),
        user_id: row.get("user_id"),
        connection_id: row.get("connection_id"),
        title: row.get("title"),
        summary: row.get("summary"),
        content: row.get("content"),
        url: row.get("url"),
        source: row.get("source"),
        doc_type: row.get("doc_type"),
        status: DocumentStatus::parse(&row.get::<String, _>("status")),
        metadata: row.get("metadata"),
        container_tags: row.get("container_tags"),
        token_count: row.get("token_count"),
        chunk_count: row.get("chunk_count"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

pub(crate) fn map_memory(row: &PgRow) -> Memory {
    let relation: Option<String> = row.get("relation");
    Memory {
        id: row.get("id"),
        custom_id: row.get("custom_id"),
        document_id: row.get("document_id"),
        org_id: row.get("org_id"),
        user_id: row.get("user_id"),
        memory: row.get("memory"),
        summary: row.get("summary"),
        mem_type: row.get("mem_type"),
        space_container_tag: row.get("space_container_tag"),
        version: row.get("version"),
        is_latest: row.get("is_latest"),
        parent_memory_id: row.get("parent_memory_id"),
        root_memory_id: row.get("root_memory_id"),
        relation: relation.as_deref().map(MemoryRelation::parse),
        source_count: row.get("source_count"),
        is_static: row.get("is_static"),
        is_inference: row.get("is_inference"),
        review_status: row.get("review_status"),
        is_forgotten: row.get("is_forgotten"),
        forget_reason: row.get("forget_reason"),
        forget_after: row.get("forget_after"),
        forget_batch_id: row.get("forget_batch_id"),
        event_date: row.get("event_date"),
        metadata: row.get("metadata"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
