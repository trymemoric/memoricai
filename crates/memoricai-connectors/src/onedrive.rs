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

        let resp = client
            .get("https://graph.microsoft.com/v1.0/me/drive/root/children")
            .bearer_auth(token)
            .query(&[("$top", "100")])
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
            let content = match client
                .get(format!(
                    "https://graph.microsoft.com/v1.0/me/drive/items/{id}/content"
                ))
                .bearer_auth(token)
                .send()
                .await
            {
                Ok(r) => r.text().await.unwrap_or_default(),
                Err(_) => {
                    stats.failed += 1;
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }
            match ctx.ingest(id, content, "onedrive", Some(name), None).await {
                Ok(_) => stats.processed += 1,
                Err(_) => stats.failed += 1,
            }
        }
        Ok(stats)
    }
}
