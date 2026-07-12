//! OneDrive connector using Microsoft Graph delta queries. The initial delta walk is a
//! complete recursive enumeration; later runs consume only changes and explicit tombstones.

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use memoricai_core::error::{Error, Result};
use serde_json::Value;

pub struct OneDrive;

fn graph_url(value: &str) -> Result<String> {
    let parsed = url::Url::parse(value)
        .map_err(|_| Error::BadRequest("invalid OneDrive delta cursor".into()))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("graph.microsoft.com") {
        return Err(Error::BadRequest(
            "OneDrive delta cursor must use graph.microsoft.com HTTPS".into(),
        ));
    }
    Ok(parsed.into())
}

async fn ingest_item(
    ctx: &ImportCtx<'_>,
    client: &reqwest::Client,
    token: &str,
    item: &Value,
    stats: &mut SyncStats,
) -> Result<()> {
    let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
        stats.failed += 1;
        return Ok(());
    };
    if item["deleted"].is_object() {
        if ctx
            .engine
            .db
            .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
            .await?
        {
            stats.processed += 1;
        }
        return Ok(());
    }
    if item["folder"].is_object() {
        return Ok(());
    }

    let name = item["name"].as_str().unwrap_or_default().to_string();
    let mime = item["file"]["mimeType"]
        .as_str()
        .unwrap_or("application/octet-stream")
        .to_string();
    let response = client
        .get(format!(
            "https://graph.microsoft.com/v1.0/me/drive/items/{id}/content"
        ))
        .bearer_auth(token)
        .send()
        .await
        .map_err(net)?;
    let bytes =
        match crate::response_bytes_limited(response, memoricai_engine::MAX_DOCUMENT_BYTES).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(item_id = id, %error, "failed to download OneDrive item");
                stats.failed += 1;
                return Ok(());
            }
        };
    if bytes.is_empty() {
        let _ = ctx
            .engine
            .db
            .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
            .await?;
        return Ok(());
    }
    match ctx
        .ingest_bytes(id, bytes, &name, &mime, Some(name.clone()), None)
        .await
    {
        Ok(()) => stats.processed += 1,
        Err(error) => {
            tracing::warn!(item_id = id, %error, "failed to ingest OneDrive item");
            stats.failed += 1;
        }
    }
    Ok(())
}

#[async_trait]
impl Connector for OneDrive {
    fn provider(&self) -> &'static str {
        "onedrive"
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        let mut initial = ctx.cursor.is_none();
        let mut stats = SyncStats {
            reconcile_deletions: initial,
            ..Default::default()
        };
        let limit = ctx.document_limit.max(0);
        let mut next_url = Some(match ctx.cursor.as_deref() {
            Some(cursor) => graph_url(cursor)?,
            None => "https://graph.microsoft.com/v1.0/me/drive/root/delta?$top=100".into(),
        });
        let mut delta_link = None;

        while let Some(url) = next_url.take() {
            let response = client
                .get(url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(net)?;
            if response.status() == reqwest::StatusCode::GONE && !initial {
                tracing::info!("OneDrive delta cursor expired; running a full reconciliation");
                initial = true;
                stats = SyncStats {
                    reconcile_deletions: true,
                    ..Default::default()
                };
                delta_link = None;
                next_url =
                    Some("https://graph.microsoft.com/v1.0/me/drive/root/delta?$top=100".into());
                continue;
            }
            let page: Value = crate::ensure_ok(response)
                .await?
                .json()
                .await
                .map_err(net)?;
            if let Some(items) = page["value"].as_array() {
                for item in items {
                    if initial {
                        if let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) {
                            if !item["folder"].is_object() && !item["deleted"].is_object() {
                                ctx.mark_seen(id);
                            }
                        }
                    }
                    ingest_item(ctx, &client, token, item, &mut stats).await?;
                    if initial && limit > 0 && (stats.processed + stats.failed) >= limit {
                        stats.truncated = true;
                        return Ok(stats);
                    }
                }
            }
            next_url = page["@odata.nextLink"]
                .as_str()
                .map(graph_url)
                .transpose()?;
            delta_link = page["@odata.deltaLink"]
                .as_str()
                .map(graph_url)
                .transpose()?;
        }
        stats.cursor = delta_link;
        Ok(stats)
    }
}
