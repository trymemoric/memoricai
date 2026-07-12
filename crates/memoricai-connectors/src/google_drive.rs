//! Google Drive connector. The first run enumerates all files and checkpoints a changes
//! page token; later runs consume Drive's change feed, including removed/trashed files.

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use memoricai_core::error::Result;
use serde_json::Value;

pub struct GoogleDrive;

async fn ingest_file(
    ctx: &ImportCtx<'_>,
    client: &reqwest::Client,
    token: &str,
    file: &Value,
    stats: &mut SyncStats,
) -> Result<()> {
    let Some(id) = file["id"].as_str().filter(|id| !id.is_empty()) else {
        stats.failed += 1;
        return Ok(());
    };
    if file["trashed"].as_bool().unwrap_or(false) {
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
    let name = file["name"].as_str().unwrap_or_default().to_string();
    let mime = file["mimeType"].as_str().unwrap_or_default();

    if mime == "application/pdf" {
        let response = client
            .get(format!("https://www.googleapis.com/drive/v3/files/{id}"))
            .bearer_auth(token)
            .query(&[("alt", "media")])
            .send()
            .await
            .map_err(net)?;
        let bytes =
            crate::response_bytes_limited(response, memoricai_engine::MAX_DOCUMENT_BYTES).await?;
        match ctx
            .ingest_bytes(
                id,
                bytes,
                &name,
                "application/pdf",
                Some(name.clone()),
                None,
            )
            .await
        {
            Ok(()) => stats.processed += 1,
            Err(error) => {
                tracing::warn!(file_id = id, %error, "failed to ingest Drive PDF");
                stats.failed += 1;
            }
        }
        return Ok(());
    }

    let content = if mime.starts_with("application/vnd.google-apps") {
        let response = client
            .get(format!(
                "https://www.googleapis.com/drive/v3/files/{id}/export"
            ))
            .bearer_auth(token)
            .query(&[("mimeType", "text/plain")])
            .send()
            .await
            .map_err(net)?;
        String::from_utf8(
            crate::response_bytes_limited(response, memoricai_engine::MAX_DOCUMENT_BYTES).await?,
        )
        .map_err(|_| memoricai_core::error::Error::BadRequest("Drive export is not UTF-8".into()))?
    } else if mime.starts_with("text/") {
        let response = client
            .get(format!("https://www.googleapis.com/drive/v3/files/{id}"))
            .bearer_auth(token)
            .query(&[("alt", "media")])
            .send()
            .await
            .map_err(net)?;
        String::from_utf8(
            crate::response_bytes_limited(response, memoricai_engine::MAX_DOCUMENT_BYTES).await?,
        )
        .map_err(|_| memoricai_core::error::Error::BadRequest("Drive file is not UTF-8".into()))?
    } else {
        // If a previously-indexed file changed to an unsupported binary type, remove the
        // obsolete text representation instead of retaining stale content.
        let _ = ctx
            .engine
            .db
            .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
            .await?;
        return Ok(());
    };

    if content.trim().is_empty() {
        let _ = ctx
            .engine
            .db
            .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
            .await?;
        return Ok(());
    }
    let doc_type = if mime.contains("presentation") {
        "google_slide"
    } else if mime.contains("spreadsheet") {
        "google_sheet"
    } else {
        "google_doc"
    };
    match ctx.ingest(id, content, doc_type, Some(name), None).await {
        Ok(()) => stats.processed += 1,
        Err(error) => {
            tracing::warn!(file_id = id, %error, "failed to ingest Drive file");
            stats.failed += 1;
        }
    }
    Ok(())
}

async fn initial_cursor(client: &reqwest::Client, token: &str) -> Result<String> {
    let response = client
        .get("https://www.googleapis.com/drive/v3/changes/startPageToken")
        .bearer_auth(token)
        .query(&[("supportsAllDrives", "true")])
        .send()
        .await
        .map_err(net)?;
    let value: Value = crate::ensure_ok(response)
        .await?
        .json()
        .await
        .map_err(net)?;
    value["startPageToken"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| memoricai_core::error::Error::Model("Drive omitted startPageToken".into()))
}

#[async_trait]
impl Connector for GoogleDrive {
    fn provider(&self) -> &'static str {
        "google-drive"
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        let limit = ctx.document_limit.max(0);

        if let Some(cursor) = ctx.cursor.as_deref() {
            let mut stats = SyncStats::default();
            let mut page_token = Some(cursor.to_string());
            let mut new_cursor = None;
            let mut expired = false;
            while let Some(current) = page_token.take() {
                let response = client
                    .get("https://www.googleapis.com/drive/v3/changes")
                    .bearer_auth(token)
                    .query(&[
                        ("pageToken", current),
                        ("pageSize", "100".into()),
                        ("includeRemoved", "true".into()),
                        ("supportsAllDrives", "true".into()),
                        (
                            "fields",
                            "nextPageToken,newStartPageToken,changes(fileId,removed,file(id,name,mimeType,trashed))".into(),
                        ),
                    ])
                    .send()
                    .await
                    .map_err(net)?;
                if response.status() == reqwest::StatusCode::GONE {
                    tracing::info!("Drive change cursor expired; running a full reconciliation");
                    expired = true;
                    break;
                }
                let page: Value = crate::ensure_ok(response)
                    .await?
                    .json()
                    .await
                    .map_err(net)?;
                if let Some(changes) = page["changes"].as_array() {
                    for change in changes {
                        let id = change["fileId"].as_str().unwrap_or_default();
                        if change["removed"].as_bool().unwrap_or(false) {
                            if !id.is_empty()
                                && ctx
                                    .engine
                                    .db
                                    .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
                                    .await?
                            {
                                stats.processed += 1;
                            }
                        } else {
                            ingest_file(ctx, &client, token, &change["file"], &mut stats).await?;
                        }
                    }
                }
                page_token = page["nextPageToken"].as_str().map(str::to_string);
                new_cursor = page["newStartPageToken"].as_str().map(str::to_string);
            }
            if !expired {
                stats.cursor = new_cursor;
                return Ok(stats);
            }
        }

        // Capture the high-water mark before enumeration so changes racing with the full
        // scan are replayed on the next delta run rather than missed.
        let checkpoint = initial_cursor(&client, token).await?;
        let mut stats = SyncStats {
            reconcile_deletions: true,
            ..Default::default()
        };
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![
                (
                    "q".to_string(),
                    "trashed=false and mimeType!='application/vnd.google-apps.folder'".to_string(),
                ),
                (
                    "fields".to_string(),
                    "nextPageToken,files(id,name,mimeType,trashed)".to_string(),
                ),
                ("pageSize".to_string(), "100".to_string()),
            ];
            if let Some(value) = &page_token {
                query.push(("pageToken".into(), value.clone()));
            }
            let response = client
                .get("https://www.googleapis.com/drive/v3/files")
                .bearer_auth(token)
                .query(&query)
                .send()
                .await
                .map_err(net)?;
            let page: Value = crate::ensure_ok(response)
                .await?
                .json()
                .await
                .map_err(net)?;
            if let Some(files) = page["files"].as_array() {
                for file in files {
                    if let Some(id) = file["id"].as_str().filter(|id| !id.is_empty()) {
                        ctx.mark_seen(id);
                    }
                    ingest_file(ctx, &client, token, file, &mut stats).await?;
                    if limit > 0 && (stats.processed + stats.failed) >= limit {
                        stats.truncated = true;
                        return Ok(stats);
                    }
                }
            }
            page_token = page["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        stats.cursor = Some(checkpoint);
        Ok(stats)
    }
}
