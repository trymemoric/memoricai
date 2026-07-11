//! Granola connector: API-key auth (no OAuth). Imports meeting notes/transcripts.
//! Sync is initial + cron polling (no webhooks).

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use memoricai_core::error::{Error, Result};
use serde_json::Value;

pub struct Granola;

#[async_trait]
impl Connector for Granola {
    fn provider(&self) -> &'static str {
        "granola"
    }
    fn is_oauth(&self) -> bool {
        false
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let api_key = ctx.metadata["apiKey"].as_str().ok_or_else(|| {
            Error::BadRequest("granola connection requires metadata.apiKey".into())
        })?;
        let base = std::env::var("MEMORICAI_GRANOLA_BASE_URL")
            .unwrap_or_else(|_| "https://api.granola.ai".to_string());
        let client = http();
        let mut stats = SyncStats::default();

        let resp = client
            .get(format!("{}/v1/documents", base.trim_end_matches('/')))
            .bearer_auth(api_key)
            .send()
            .await
            .map_err(net)?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized("granola api key rejected".into()));
        }
        let v: Value = resp.json().await.map_err(net)?;
        let empty = vec![];
        let docs = v["documents"]
            .as_array()
            .or_else(|| v.as_array())
            .unwrap_or(&empty);
        for d in docs {
            let title = d["title"].as_str().unwrap_or("Granola note").to_string();
            let external_id = d["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| title.clone());
            let content = d["transcript"]
                .as_str()
                .or_else(|| d["notes"].as_str())
                .or_else(|| d["content"].as_str())
                .unwrap_or_default()
                .to_string();
            if content.trim().is_empty() {
                continue;
            }
            match ctx
                .ingest(&external_id, content, "granola", Some(title), None)
                .await
            {
                Ok(_) => stats.processed += 1,
                Err(_) => stats.failed += 1,
            }
        }
        Ok(stats)
    }
}
