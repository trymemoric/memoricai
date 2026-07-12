//! Gmail connector. The initial INBOX enumeration stores a history id; subsequent syncs
//! consume Gmail history and reconcile additions, removals from INBOX, and deletions.

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use base64::Engine as _;
use memoricai_core::error::{Error, Result};
use serde_json::Value;
use std::collections::HashSet;

pub struct Gmail;

fn decode_b64url(value: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

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
        for part in parts {
            out.push_str(&extract_body(part));
            out.push('\n');
        }
    }
    out
}

async fn fetch_message(client: &reqwest::Client, token: &str, id: &str) -> Result<Option<Value>> {
    let response = client
        .get(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}"
        ))
        .bearer_auth(token)
        .query(&[("format", "full")])
        .send()
        .await
        .map_err(net)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(
        crate::ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(net)?,
    ))
}

async fn sync_message(
    ctx: &ImportCtx<'_>,
    client: &reqwest::Client,
    token: &str,
    id: &str,
    stats: &mut SyncStats,
) -> Result<()> {
    let Some(message) = fetch_message(client, token, id).await? else {
        if ctx
            .engine
            .db
            .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
            .await?
        {
            stats.processed += 1;
        }
        return Ok(());
    };
    let in_inbox = message["labelIds"]
        .as_array()
        .is_some_and(|labels| labels.iter().any(|label| label.as_str() == Some("INBOX")));
    if !in_inbox {
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

    let subject = message["payload"]["headers"]
        .as_array()
        .and_then(|headers| {
            headers
                .iter()
                .find(|header| header["name"].as_str() == Some("Subject"))
        })
        .and_then(|header| header["value"].as_str())
        .unwrap_or("(no subject)")
        .to_string();
    let body = extract_body(&message["payload"]);
    let content = if body.trim().is_empty() {
        message["snippet"].as_str().unwrap_or_default().to_string()
    } else {
        body
    };
    if content.trim().is_empty() {
        let _ = ctx
            .engine
            .db
            .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
            .await?;
        return Ok(());
    }
    match ctx
        .ingest(id, content, "gmail_message", Some(subject), None)
        .await
    {
        Ok(()) => stats.processed += 1,
        Err(error) => {
            tracing::warn!(message_id = id, %error, "failed to ingest Gmail message");
            stats.failed += 1;
        }
    }
    Ok(())
}

async fn current_history_id(client: &reqwest::Client, token: &str) -> Result<String> {
    let response = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
        .bearer_auth(token)
        .send()
        .await
        .map_err(net)?;
    let profile: Value = crate::ensure_ok(response)
        .await?
        .json()
        .await
        .map_err(net)?;
    profile["historyId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Model("Gmail omitted profile historyId".into()))
}

async fn full_sync(
    ctx: &ImportCtx<'_>,
    client: &reqwest::Client,
    token: &str,
) -> Result<SyncStats> {
    let checkpoint = current_history_id(client, token).await?;
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
        if let Some(value) = &page_token {
            query.push(("pageToken".into(), value.clone()));
        }
        let response = client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
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
        if let Some(messages) = page["messages"].as_array() {
            for message in messages {
                let Some(id) = message["id"].as_str().filter(|id| !id.is_empty()) else {
                    stats.failed += 1;
                    continue;
                };
                ctx.mark_seen(id);
                sync_message(ctx, client, token, id, &mut stats).await?;
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

async fn incremental_sync(
    ctx: &ImportCtx<'_>,
    client: &reqwest::Client,
    token: &str,
    cursor: &str,
) -> Result<Option<SyncStats>> {
    let mut stats = SyncStats::default();
    let mut page_token: Option<String> = None;
    let mut changed = HashSet::new();
    let mut deleted = HashSet::new();
    let mut checkpoint = cursor.to_string();
    loop {
        let mut query = vec![
            ("startHistoryId".to_string(), cursor.to_string()),
            ("maxResults".to_string(), "100".to_string()),
            ("labelId".to_string(), "INBOX".to_string()),
        ];
        if let Some(value) = &page_token {
            query.push(("pageToken".into(), value.clone()));
        }
        let response = client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/history")
            .bearer_auth(token)
            .query(&query)
            .send()
            .await
            .map_err(net)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            // Gmail expires old history ids. Falling back to a complete enumeration is
            // required to recover without silently losing changes.
            return Ok(None);
        }
        let page: Value = crate::ensure_ok(response)
            .await?
            .json()
            .await
            .map_err(net)?;
        if let Some(history) = page["history"].as_array() {
            for entry in history {
                if let Some(messages) = entry["messages"].as_array() {
                    changed.extend(
                        messages
                            .iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_string)),
                    );
                }
                if let Some(messages) = entry["messagesDeleted"].as_array() {
                    deleted.extend(
                        messages
                            .iter()
                            .filter_map(|m| m["message"]["id"].as_str().map(str::to_string)),
                    );
                }
            }
        }
        if let Some(history_id) = page["historyId"].as_str() {
            checkpoint = history_id.to_string();
        }
        page_token = page["nextPageToken"].as_str().map(str::to_string);
        if page_token.is_none() {
            break;
        }
    }

    for id in &deleted {
        changed.remove(id);
        if ctx
            .engine
            .db
            .delete_connection_document(&ctx.org_id, &ctx.connection_id, id)
            .await?
        {
            stats.processed += 1;
        }
    }
    for id in changed {
        sync_message(ctx, client, token, &id, &mut stats).await?;
    }
    stats.cursor = Some(checkpoint);
    Ok(Some(stats))
}

#[async_trait]
impl Connector for Gmail {
    fn provider(&self) -> &'static str {
        "gmail"
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        if let Some(cursor) = ctx.cursor.as_deref() {
            if let Some(stats) = incremental_sync(ctx, &client, token, cursor).await? {
                return Ok(stats);
            }
        }
        full_sync(ctx, &client, token).await
    }
}
