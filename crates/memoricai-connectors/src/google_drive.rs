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
        let mut stats = SyncStats::default();

        let resp = client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(token)
            .query(&[
                (
                    "q",
                    "trashed=false and mimeType!='application/vnd.google-apps.folder'",
                ),
                ("fields", "files(id,name,mimeType)"),
                ("pageSize", "100"),
            ])
            .send()
            .await
            .map_err(net)?;
        let v: Value = crate::ensure_ok(resp).await?.json().await.map_err(net)?;
        let empty = vec![];
        for f in v["files"].as_array().unwrap_or(&empty) {
            let id = f["id"].as_str().unwrap_or_default();
            let name = f["name"].as_str().unwrap_or_default().to_string();
            let mime = f["mimeType"].as_str().unwrap_or_default();

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
            } else if mime == "application/pdf" || mime.starts_with("text/") {
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
                continue;
            };

            if content.trim().is_empty() {
                continue;
            }
            let dt = if mime.contains("presentation") {
                "google_slide"
            } else if mime.contains("spreadsheet") {
                "google_sheet"
            } else if mime == "application/pdf" {
                "pdf"
            } else {
                "google_doc"
            };
            match ctx.ingest(id, content, dt, Some(name), None).await {
                Ok(_) => stats.processed += 1,
                Err(_) => stats.failed += 1,
            }
        }
        Ok(stats)
    }
}
