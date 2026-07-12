//! Gmail connector: ingests INBOX message bodies. Pub/Sub watch is not
//! registered; the 4h cron polls for freshness.

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use base64::Engine as _;
use memoricai_core::error::Result;
use serde_json::Value;

pub struct Gmail;

fn decode_b64url(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

/// Walk a Gmail payload tree collecting text/plain parts.
fn extract_body(payload: &Value) -> String {
    let mut out = String::new();
    if let Some(data) = payload["body"]["data"].as_str() {
        if payload["mimeType"].as_str() == Some("text/plain")
            || payload["mimeType"].as_str().is_none()
        {
            out.push_str(&decode_b64url(data));
        }
    }
    if let Some(parts) = payload["parts"].as_array() {
        for p in parts {
            out.push_str(&extract_body(p));
            out.push('\n');
        }
    }
    out
}

#[async_trait]
impl Connector for Gmail {
    fn provider(&self) -> &'static str {
        "gmail"
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
                ("maxResults".to_string(), "100".to_string()),
                ("labelIds".to_string(), "INBOX".to_string()),
            ];
            if let Some(pt) = &page_token {
                query.push(("pageToken".to_string(), pt.clone()));
            }
            let resp = client
                .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
                .bearer_auth(token)
                .query(&query)
                .send()
                .await
                .map_err(net)?;
            let list: Value = crate::ensure_ok(resp).await?.json().await.map_err(net)?;

            let empty = vec![];
            for m in list["messages"].as_array().unwrap_or(&empty) {
                let id = m["id"].as_str().unwrap_or_default();
                ctx.mark_seen(id);
                let msg: Value = match client
                    .get(format!(
                        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}"
                    ))
                    .bearer_auth(token)
                    .query(&[("format", "full")])
                    .send()
                    .await
                {
                    Ok(r) => r.json().await.unwrap_or(Value::Null),
                    Err(_) => {
                        stats.failed += 1;
                        continue;
                    }
                };
                let subject = msg["payload"]["headers"]
                    .as_array()
                    .and_then(|hs| hs.iter().find(|h| h["name"].as_str() == Some("Subject")))
                    .and_then(|h| h["value"].as_str())
                    .unwrap_or("(no subject)")
                    .to_string();
                let body = extract_body(&msg["payload"]);
                let content = if body.trim().is_empty() {
                    msg["snippet"].as_str().unwrap_or_default().to_string()
                } else {
                    body
                };
                if content.trim().is_empty() {
                    continue;
                }
                match ctx
                    .ingest(id, content, "gmail_thread", Some(subject), None)
                    .await
                {
                    Ok(_) => stats.processed += 1,
                    Err(_) => stats.failed += 1,
                }
            }
            page_token = list["nextPageToken"].as_str().map(str::to_string);
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
