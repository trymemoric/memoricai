//! OneDrive connector (Microsoft Graph): ingests drive items. Graph change
//! subscriptions aren't registered; the 4h cron polls for freshness.

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use memoricai_core::error::Result;
use serde_json::Value;

pub struct OneDrive;

#[async_trait]
impl Connector for OneDrive {
    fn provider(&self) -> &'static str {
        "onedrive"
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        let mut stats = SyncStats::default();
        let limit = ctx.document_limit.max(0);
        let mut next_url =
            Some("https://graph.microsoft.com/v1.0/me/drive/root/children?$top=100".to_string());

        while let Some(url) = next_url.take() {
            let resp = client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(net)?;
            let list: Value = crate::ensure_ok(resp).await?.json().await.map_err(net)?;

            let empty = vec![];
            for item in list["value"].as_array().unwrap_or(&empty) {
                if item["folder"].is_object() {
                    continue;
                }
                let id = item["id"].as_str().unwrap_or_default();
                let name = item["name"].as_str().unwrap_or_default().to_string();
                let mime = item["file"]["mimeType"].as_str().unwrap_or("").to_string();
                let resp = match client
                    .get(format!(
                        "https://graph.microsoft.com/v1.0/me/drive/items/{id}/content"
                    ))
                    .bearer_auth(token)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        stats.failed += 1;
                        continue;
                    }
                };
                // Download raw bytes and extract via the binary/media extractor, instead of
                // decoding arbitrary Office/binary files as lossy UTF-8 text.
                let bytes = match crate::ensure_ok(resp).await.map(|r| r.bytes()) {
                    Ok(fut) => match fut.await {
                        Ok(b) => b,
                        Err(_) => {
                            stats.failed += 1;
                            continue;
                        }
                    },
                    Err(_) => {
                        stats.failed += 1;
                        continue;
                    }
                };
                if bytes.is_empty() {
                    continue;
                }
                match ctx
                    .ingest_bytes(id, bytes.to_vec(), &name, &mime, Some(name.clone()), None)
                    .await
                {
                    Ok(_) => stats.processed += 1,
                    Err(_) => stats.failed += 1,
                }
            }
            next_url = list["@odata.nextLink"].as_str().map(str::to_string);
            if limit > 0 && (stats.processed + stats.failed) >= limit {
                break;
            }
        }
        Ok(stats)
    }
}
