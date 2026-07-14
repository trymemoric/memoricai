//! memoricai-connectors: external integrations (Google Drive, Gmail, Notion,
//! OneDrive, GitHub, Granola, web-crawler, S3). A `Connector` trait + a shared
//! sync engine (SyncRun ledger) + a `Connectors` facade the API + cron call.

pub mod github;
pub mod gmail;
pub mod google_drive;
pub mod granola;
pub mod notion;
pub mod oauth;
pub mod onedrive;
pub mod s3;
pub mod sync;
pub mod web_crawler;

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use memoricai_core::dto::{CreateConnectionRequest, CreateConnectionResponse, IngestRequest};
use memoricai_core::error::{Error, Result};
use memoricai_core::model::{Connection, SyncRun};
use memoricai_db::connections::ConnState;
use memoricai_engine::Engine;
use serde_json::{json, Value};

pub const SUPPORTED: &[&str] = &[
    "google-drive",
    "gmail",
    "notion",
    "onedrive",
    "github",
    "granola",
    "web-crawler",
    "s3",
];

/// OAuth token material returned by a provider.
pub struct TokenSet {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub email: Option<String>,
}

/// Counters recorded for a sync run.
#[derive(Default)]
pub struct SyncStats {
    pub processed: i32,
    pub failed: i32,
    pub cursor: Option<String>,
    /// Set when the import stopped before fully enumerating the source (e.g. hit the
    /// document limit). Deletion reconciliation is skipped for truncated runs.
    pub truncated: bool,
    /// Opt-in: the connector fully enumerated the source and marked every seen id, so
    /// documents no longer present may be reconciled (deleted). Off by default so
    /// partially-enumerating connectors never trigger deletion.
    pub reconcile_deletions: bool,
}

/// Everything a provider needs to import for one connection.
pub struct ImportCtx<'a> {
    pub engine: &'a Engine,
    pub org_id: String,
    pub user_id: Option<String>,
    pub connection_id: String,
    pub document_limit: i32,
    pub container_tags: Vec<String>,
    pub access_token: Option<String>,
    pub metadata: Value,
    pub cursor: Option<String>,
    /// Reserved document count for this sync. Keeping it in the import context
    /// avoids a `count(*)` query for every provider item.
    pub document_count: std::sync::Arc<tokio::sync::Mutex<i64>>,
    /// External ids enumerated at the source this run, for deletion reconciliation.
    pub seen: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl ImportCtx<'_> {
    /// Record that `external_id` currently exists at the source (call for every enumerated
    /// item, including ones that are skipped), so reconciliation only deletes what is gone.
    pub fn mark_seen(&self, external_id: &str) {
        self.seen.lock().unwrap().insert(external_id.to_string());
    }

    /// Ingest one fetched item as a document attributed to this connection.
    pub async fn ingest(
        &self,
        external_id: &str,
        content: String,
        doc_type: &str,
        title: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<()> {
        self.mark_seen(external_id);
        let container_tags = tags.or_else(|| {
            if self.container_tags.is_empty() {
                None
            } else {
                Some(self.container_tags.clone())
            }
        });
        let custom_id = format!("{}:{external_id}", self.connection_id);
        let is_new = !self
            .engine
            .db
            .document_exists_by_custom_id(&self.org_id, &custom_id)
            .await?;
        if is_new {
            let mut count = self.document_count.lock().await;
            if *count >= i64::from(self.document_limit.max(0)) {
                return Err(Error::BadRequest(
                    "connection document limit reached".into(),
                ));
            }
            *count += 1;
        }
        let req = IngestRequest {
            content,
            custom_id: Some(custom_id),
            container_tag: None,
            container_tags,
            metadata: Some(json!({ "mc_source": self.connection_id })),
            entity_context: None,
            content_type: Some(doc_type.to_string()),
            title,
            raw: None,
        };
        let result = self
            .engine
            .ingest_from_connection(
                &self.org_id,
                self.user_id.as_deref(),
                &req,
                &self.connection_id,
                "connection",
            )
            .await;
        if result.is_err() && is_new {
            let mut count = self.document_count.lock().await;
            *count = count.saturating_sub(1);
        }
        result.map(|_| ())
    }

    /// Ingest one fetched *binary* item: extract text via the media/binary extractor
    /// (PDF/image/audio) instead of decoding raw bytes as lossy UTF-8 text.
    pub async fn ingest_bytes(
        &self,
        external_id: &str,
        bytes: Vec<u8>,
        filename: &str,
        mime: &str,
        title: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<()> {
        let (content, doc_type) = self.engine.extract_file(&bytes, filename, mime).await?;
        self.ingest(external_id, content, &doc_type, title, tags)
            .await
    }

    pub fn token(&self) -> Result<&str> {
        self.access_token
            .as_deref()
            .ok_or_else(|| Error::BadRequest("connection is missing an access token".into()))
    }
}

/// A single integration.
#[async_trait]
pub trait Connector: Send + Sync {
    fn provider(&self) -> &'static str;
    fn is_oauth(&self) -> bool {
        true
    }
    /// Import documents. Returns counters + an optional new incremental cursor.
    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats>;
    /// List selectable resources (GitHub repos).
    async fn resources(&self, _ctx: &ImportCtx<'_>, _page: u32, _per_page: u32) -> Result<Value> {
        Err(Error::BadRequest(
            "resources not supported for this provider".into(),
        ))
    }
    /// Configure selected resources (GitHub).
    async fn configure(&self, _ctx: &ImportCtx<'_>, _resources: Value) -> Result<Value> {
        Err(Error::BadRequest(
            "configure not supported for this provider".into(),
        ))
    }
    /// Handle a provider webhook. Returns `None` to trigger no sync, or `Some(scope)` to
    /// sync only connections matching `scope` (e.g. a specific repo `full_name`); an empty
    /// scope means all of this provider's connections.
    async fn handle_webhook(
        &self,
        _headers: &HashMap<String, String>,
        _body: &[u8],
    ) -> Result<Option<String>> {
        Ok(None)
    }
}

fn connector_for(provider: &str) -> Result<Box<dyn Connector>> {
    Ok(match provider {
        "google-drive" => Box::new(google_drive::GoogleDrive),
        "gmail" => Box::new(gmail::Gmail),
        "notion" => Box::new(notion::Notion),
        "onedrive" => Box::new(onedrive::OneDrive),
        "github" => Box::new(github::GitHub),
        "granola" => Box::new(granola::Granola),
        "web-crawler" => Box::new(web_crawler::WebCrawler),
        "s3" => Box::new(s3::S3),
        _ => return Err(Error::BadRequest(format!("unknown provider: {provider}"))),
    })
}

#[derive(Clone)]
pub struct Connectors {
    pub engine: Engine,
}

impl Connectors {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }

    pub fn supported() -> &'static [&'static str] {
        SUPPORTED
    }

    pub async fn create(
        &self,
        org_id: &str,
        user_id: Option<&str>,
        provider: &str,
        req: &CreateConnectionRequest,
        base_url: &str,
    ) -> Result<CreateConnectionResponse> {
        let connector = connector_for(provider)?;
        let tags = req.container_tags.clone().unwrap_or_default();
        let document_limit = req.document_limit.unwrap_or(10000);
        let metadata = req.metadata.clone().unwrap_or_else(|| json!({}));
        if tags.len() > 20 {
            return Err(Error::BadRequest(
                "at most 20 container tags are allowed".into(),
            ));
        }
        let mut unique_tags = std::collections::HashSet::with_capacity(tags.len());
        for tag in &tags {
            if !memoricai_core::is_valid_container_tag(tag) {
                return Err(Error::BadRequest(format!("invalid container tag: {tag}")));
            }
            if !unique_tags.insert(tag) {
                return Err(Error::BadRequest(format!("duplicate container tag: {tag}")));
            }
        }
        if !(1..=100_000).contains(&document_limit) {
            return Err(Error::BadRequest(
                "documentLimit must be between 1 and 100000".into(),
            ));
        }
        if serde_json::to_vec(&metadata)
            .map_err(|error| Error::BadRequest(format!("invalid metadata: {error}")))?
            .len()
            > 256 * 1024
        {
            return Err(Error::BadRequest("metadata exceeds 256 KiB".into()));
        }
        if let Some(redirect) = &req.redirect_url {
            let url = url::Url::parse(redirect)
                .map_err(|_| Error::BadRequest("redirectUrl must be an absolute URL".into()))?;
            let loopback = matches!(
                url.host_str(),
                Some("localhost") | Some("127.0.0.1") | Some("::1")
            );
            if redirect.len() > 2048
                || url.username() != ""
                || url.password().is_some()
                || url.fragment().is_some()
                || (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            {
                return Err(Error::BadRequest(
                    "redirectUrl must be HTTPS (or loopback HTTP) without credentials or fragment"
                        .into(),
                ));
            }
        }

        if connector.is_oauth() {
            let state = memoricai_core::ids::token(32);
            let cfg = oauth::provider_config(provider)
                .ok_or_else(|| Error::BadRequest("provider has no oauth config".into()))?;
            let client_id = oauth::client_id(provider)?;
            let auth_url = oauth::authorize_url(&cfg, base_url, provider, &state, &client_id);
            self.engine
                .db
                .insert_connection_state(&ConnState {
                    state_token: state,
                    provider: provider.to_string(),
                    org_id: org_id.to_string(),
                    user_id: user_id.map(|s| s.to_string()),
                    redirect_url: req.redirect_url.clone(),
                    container_tags: tags,
                    document_limit,
                    metadata,
                    expires_at: Utc::now() + chrono::Duration::minutes(10),
                })
                .await?;
            Ok(CreateConnectionResponse {
                id: String::new(),
                auth_link: Some(auth_url),
                expires_in: Some("600".to_string()),
                redirects_to: req.redirect_url.clone(),
            })
        } else {
            let id = memoricai_core::ids::connection_id();
            self.engine
                .db
                .insert_connection(
                    &id,
                    provider,
                    org_id,
                    user_id,
                    &tags,
                    document_limit,
                    &metadata,
                )
                .await?;
            let _ = self.import(org_id, &id, "manual").await;
            Ok(CreateConnectionResponse {
                id,
                auth_link: None,
                expires_in: None,
                redirects_to: None,
            })
        }
    }

    pub async fn oauth_callback(&self, provider: &str, code: &str, state: &str) -> Result<String> {
        let st = self
            .engine
            .db
            .take_connection_state(state)
            .await?
            .ok_or_else(|| Error::BadRequest("invalid or expired oauth state".into()))?;
        if st.provider != provider {
            return Err(Error::BadRequest(
                "provider mismatch in oauth callback".into(),
            ));
        }
        let tokens = oauth::exchange_code(provider, code).await?;
        let id = memoricai_core::ids::connection_id();
        self.engine
            .db
            .insert_connection(
                &id,
                provider,
                &st.org_id,
                st.user_id.as_deref(),
                &st.container_tags,
                st.document_limit,
                &st.metadata,
            )
            .await?;
        self.engine
            .db
            .update_connection_tokens(
                &id,
                Some(&tokens.access),
                tokens.refresh.as_deref(),
                tokens.expires_at,
                tokens.email.as_deref(),
            )
            .await?;
        let _ = self.import(&st.org_id, &id, "manual").await;
        Ok(st.redirect_url.unwrap_or_else(|| "/".to_string()))
    }

    pub async fn import(&self, org_id: &str, connection_id: &str, trigger: &str) -> Result<()> {
        let conn = self
            .engine
            .db
            .get_connection(org_id, connection_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("connection {connection_id}")))?;
        let mut creds = self
            .engine
            .db
            .get_connection_credentials(connection_id)
            .await?;

        if let Some(c) = &creds {
            if let (Some(exp), Some(refresh)) = (c.expires_at, c.refresh_token.as_deref()) {
                if exp < Utc::now() {
                    if let Ok(t) = oauth::refresh(&conn.provider, refresh).await {
                        self.engine
                            .db
                            .update_connection_tokens(
                                connection_id,
                                Some(&t.access),
                                t.refresh.as_deref(),
                                t.expires_at,
                                None,
                            )
                            .await?;
                        creds = self
                            .engine
                            .db
                            .get_connection_credentials(connection_id)
                            .await?;
                    }
                }
            }
        }

        let connector = connector_for(&conn.provider)?;
        let document_count = self
            .engine
            .db
            .count_documents_for_connection(org_id, connection_id)
            .await?;
        let ctx = ImportCtx {
            engine: &self.engine,
            org_id: org_id.to_string(),
            user_id: conn.user_id.clone(),
            connection_id: connection_id.to_string(),
            document_limit: conn.document_limit,
            container_tags: conn.container_tags.clone(),
            access_token: creds.as_ref().and_then(|c| c.access_token.clone()),
            metadata: conn.metadata.clone(),
            cursor: creds.as_ref().and_then(|c| c.sync_cursor.clone()),
            document_count: std::sync::Arc::new(tokio::sync::Mutex::new(document_count)),
            seen: Default::default(),
        };

        sync::run(
            &self.engine.db,
            connection_id,
            trigger,
            connector.as_ref(),
            &ctx,
        )
        .await
    }

    pub async fn list(
        &self,
        org_id: &str,
        provider: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Vec<Connection>> {
        self.engine
            .db
            .list_connections(org_id, provider, tags)
            .await
    }

    pub async fn get(&self, org_id: &str, id: &str) -> Result<Option<Connection>> {
        self.engine.db.get_connection(org_id, id).await
    }

    pub async fn delete(&self, org_id: &str, id: &str, delete_documents: bool) -> Result<bool> {
        self.engine
            .db
            .delete_connection(org_id, id, delete_documents)
            .await
    }

    pub async fn sync_runs(&self, org_id: &str, connection_id: &str) -> Result<Vec<SyncRun>> {
        self.engine
            .db
            .get_connection(org_id, connection_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("connection {connection_id}")))?;
        self.engine.db.list_sync_runs(connection_id).await
    }

    pub async fn resources(
        &self,
        org_id: &str,
        connection_id: &str,
        page: u32,
        per_page: u32,
    ) -> Result<Value> {
        let (conn, ctx) = self.ctx_for(org_id, connection_id).await?;
        connector_for(&conn.provider)?
            .resources(&ctx, page, per_page)
            .await
    }

    pub async fn configure(
        &self,
        org_id: &str,
        connection_id: &str,
        resources: Value,
    ) -> Result<Value> {
        let (conn, ctx) = self.ctx_for(org_id, connection_id).await?;
        let out = connector_for(&conn.provider)?
            .configure(&ctx, resources)
            .await?;
        let _ = self.import(org_id, connection_id, "manual").await;
        Ok(out)
    }

    pub async fn handle_webhook(
        &self,
        provider: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Result<()> {
        let connector = connector_for(provider)?;
        let Some(scope) = connector.handle_webhook(headers, body).await? else {
            return Ok(());
        };
        let due = self.engine.db.connections_due(0).await?;
        futures::stream::iter(due.into_iter().filter(|c| c.provider == provider))
            .map(|connection| {
                let scope = scope.clone();
                async move {
                    if !scope.is_empty() {
                        // Only sync connections configured to watch this webhook's resource.
                        let watches = self
                            .engine
                            .db
                            .get_connection_credentials(&connection.id)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|credentials| credentials.sync_cursor)
                            .and_then(|cursor| serde_json::from_str::<Vec<String>>(&cursor).ok())
                            .is_some_and(|resources| resources.contains(&scope));
                        if !watches {
                            return;
                        }
                    }
                    let _ = self
                        .import(&connection.org_id, &connection.id, "event")
                        .await;
                }
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
        Ok(())
    }

    pub async fn run_due_syncs(&self, hours: i64) -> Result<usize> {
        let due = self.engine.db.connections_due(hours).await?;
        let outcomes = futures::stream::iter(due)
            .map(|connection| async move {
                self.import(&connection.org_id, &connection.id, "cron")
                    .await
                    .is_ok()
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
        Ok(outcomes.into_iter().filter(|success| *success).count())
    }

    async fn ctx_for(
        &self,
        org_id: &str,
        connection_id: &str,
    ) -> Result<(Connection, ImportCtx<'_>)> {
        let conn = self
            .engine
            .db
            .get_connection(org_id, connection_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("connection {connection_id}")))?;
        let creds = self
            .engine
            .db
            .get_connection_credentials(connection_id)
            .await?;
        let document_count = self
            .engine
            .db
            .count_documents_for_connection(org_id, connection_id)
            .await?;
        let ctx = ImportCtx {
            engine: &self.engine,
            org_id: org_id.to_string(),
            user_id: conn.user_id.clone(),
            connection_id: connection_id.to_string(),
            document_limit: conn.document_limit,
            container_tags: conn.container_tags.clone(),
            access_token: creds.as_ref().and_then(|c| c.access_token.clone()),
            metadata: conn.metadata.clone(),
            cursor: creds.as_ref().and_then(|c| c.sync_cursor.clone()),
            document_count: std::sync::Arc::new(tokio::sync::Mutex::new(document_count)),
            seen: Default::default(),
        };
        Ok((conn, ctx))
    }
}

pub(crate) fn http() -> reqwest::Client {
    static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .user_agent("memoricai-connectors/0.1")
            .build()
            .expect("reqwest client")
    });
    CLIENT.clone()
}

pub(crate) fn net(e: reqwest::Error) -> Error {
    Error::Model(e.to_string())
}

/// Reject a non-2xx provider response instead of parsing its error body as data.
///
/// Without this, an `{"message":"Bad credentials"}` / `{"error":{...}}` body parses
/// as valid JSON, the expected array is absent, and the sync loop reports a
/// "completed" run of zero items — silently masking auth failures and rate limits.
pub(crate) async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let mut stream = resp.bytes_stream();
    let mut body = Vec::new();
    const MAX_ERROR_BODY: usize = 8 * 1024;
    while body.len() < MAX_ERROR_BODY {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let chunk = chunk.unwrap_or_default();
        let remaining = MAX_ERROR_BODY.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    let snippet: String = String::from_utf8_lossy(&body).chars().take(200).collect();
    Err(match status.as_u16() {
        401 | 403 => Error::Unauthorized(format!(
            "connector credentials rejected by provider ({status})"
        )),
        429 => Error::RateLimited,
        _ => Error::Model(format!("connector request failed ({status}): {snippet}")),
    })
}

/// Read a successful provider response with a hard byte ceiling. Connector content is
/// untrusted remote input; content-length alone is insufficient because chunked responses
/// can omit or lie about it.
pub(crate) async fn response_bytes_limited(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let response = ensure_ok(response).await?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(Error::BadRequest("connector file exceeds 10 MiB".into()));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(net)?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(Error::BadRequest("connector file exceeds 10 MiB".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
