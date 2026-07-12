//! Google Drive connector: exports Docs/Sheets/Slides as text and ingests them.
//! Push channels are not registered; freshness comes from the 4h cron (polling).

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use memoricai_core::error::Result;
use serde_json::Value;

pub struct GoogleDrive;

#[async_trait]
impl Connector for GoogleDrive {
    fn provider(&self) -> &'static str {
        "google-drive"
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        let mut stats = SyncStats {
            reconcile_deletions: true,
            ..Default::default()
        };
        let limit = ctx.document_limit.max(0);
        let mut page_token: Option<String> = None;

        loop {
            let mut query = vec![
                (
                    "q".to_string(),
                    "trashed=false and mimeType!='application/vnd.google-apps.folder'".to_string(),
                ),
                (
                    "fields".to_string(),
                    "nextPageToken,files(id,name,mimeType)".to_string(),
                ),
                ("pageSize".to_string(), "100".to_string()),
            ];
            if let Some(pt) = &page_token {
                query.push(("pageToken".to_string(), pt.clone()));
            }
            let resp = client
                .get("https://www.googleapis.com/drive/v3/files")
                .bearer_auth(token)
                .query(&query)
                .send()
                .await
                .map_err(net)?;
            let v: Value = crate::ensure_ok(resp).await?.json().await.map_err(net)?;
            let empty = vec![];
            for f in v["files"].as_array().unwrap_or(&empty) {
                let id = f["id"].as_str().unwrap_or_default();
                ctx.mark_seen(id);
                let name = f["name"].as_str().unwrap_or_default().to_string();
                let mime = f["mimeType"].as_str().unwrap_or_default();

                if mime == "application/pdf" {
                    // Download the real PDF bytes and extract them, rather than decoding the
                    // binary as lossy UTF-8 text.
                    let resp = client
                        .get(format!("https://www.googleapis.com/drive/v3/files/{id}"))
                        .bearer_auth(token)
                        .query(&[("alt", "media")])
                        .send()
                        .await
                        .map_err(net)?;
                    let bytes = crate::ensure_ok(resp).await?.bytes().await.map_err(net)?;
                    match ctx
                        .ingest_bytes(
                            id,
                            bytes.to_vec(),
                            &name,
                            "application/pdf",
                            Some(name.clone()),
                            None,
                        )
                        .await
                    {
                        Ok(_) => stats.processed += 1,
                        Err(_) => stats.failed += 1,
                    }
                    continue;
                }

                let content = if mime.starts_with("application/vnd.google-apps") {
                    client
                        .get(format!(
                            "https://www.googleapis.com/drive/v3/files/{id}/export"
                        ))
                        .bearer_auth(token)
                        .query(&[("mimeType", "text/plain")])
                        .send()
                        .await
                        .map_err(net)?
                        .text()
                        .await
                        .unwrap_or_default()
                } else if mime.starts_with("text/") {
                    client
                        .get(format!("https://www.googleapis.com/drive/v3/files/{id}"))
                        .bearer_auth(token)
                        .query(&[("alt", "media")])
                        .send()
                        .await
                        .map_err(net)?
                        .text()
                        .await
                        .unwrap_or_default()
                } else {
                    // Unsupported binary type: skip rather than index it as corrupted text.
                    continue;
                };

                if content.trim().is_empty() {
                    continue;
                }
                let dt = if mime.contains("presentation") {
                    "google_slide"
                } else if mime.contains("spreadsheet") {
                    "google_sheet"
                } else {
                    "google_doc"
                };
                match ctx.ingest(id, content, dt, Some(name), None).await {
                    Ok(_) => stats.processed += 1,
                    Err(_) => stats.failed += 1,
                }
            }
            page_token = v["nextPageToken"].as_str().map(str::to_string);
            if limit > 0 && (stats.processed + stats.failed) >= limit {
                stats.truncated = true;
                break;
            }
            if page_token.is_none() {
                break;
            }
        }
        Ok(stats)
    }
}
