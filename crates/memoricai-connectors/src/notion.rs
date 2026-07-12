//! Notion connector: searches pages, renders block trees to markdown, ingests.
//! Notion webhooks aren't subscribed; the 4h cron polls (last-edited filter).

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use futures::future::BoxFuture;
use memoricai_core::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub struct Notion;

const NOTION_VERSION: &str = "2022-06-28";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotionCursor {
    last_edited: DateTime<Utc>,
    last_full_at: DateTime<Utc>,
}

fn parse_cursor(value: Option<&str>) -> Option<NotionCursor> {
    let value = value?;
    serde_json::from_str(value).ok().or_else(|| {
        DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|time| NotionCursor {
                last_edited: time.with_timezone(&Utc),
                last_full_at: Utc::now() - Duration::days(2),
            })
    })
}

fn rich_text(arr: &Value) -> String {
    arr.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["plain_text"].as_str())
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn render_block(b: &Value) -> String {
    let ty = b["type"].as_str().unwrap_or_default();
    let inner = &b[ty];
    match ty {
        "heading_1" => format!("# {}\n", rich_text(&inner["rich_text"])),
        "heading_2" => format!("## {}\n", rich_text(&inner["rich_text"])),
        "heading_3" => format!("### {}\n", rich_text(&inner["rich_text"])),
        "bulleted_list_item" | "numbered_list_item" => {
            format!("- {}\n", rich_text(&inner["rich_text"]))
        }
        "to_do" => format!("- [ ] {}\n", rich_text(&inner["rich_text"])),
        "quote" => format!("> {}\n", rich_text(&inner["rich_text"])),
        "code" => format!("```\n{}\n```\n", rich_text(&inner["rich_text"])),
        "paragraph" => format!("{}\n", rich_text(&inner["rich_text"])),
        _ => {
            let t = rich_text(&inner["rich_text"]);
            if t.is_empty() {
                String::new()
            } else {
                format!("{t}\n")
            }
        }
    }
}

fn render_children<'a>(
    client: &'a reqwest::Client,
    token: &'a str,
    parent_id: &'a str,
    depth: usize,
    visited: &'a mut usize,
) -> BoxFuture<'a, Result<String>> {
    Box::pin(async move {
        if depth > 100 {
            return Err(Error::BadRequest(
                "Notion block tree exceeds 100 levels".into(),
            ));
        }
        let mut cursor: Option<String> = None;
        let mut markdown = String::new();
        loop {
            let mut query = vec![("page_size", "100".to_string())];
            if let Some(value) = &cursor {
                query.push(("start_cursor", value.clone()));
            }
            let response = client
                .get(format!(
                    "https://api.notion.com/v1/blocks/{parent_id}/children"
                ))
                .bearer_auth(token)
                .header("Notion-Version", NOTION_VERSION)
                .query(&query)
                .send()
                .await
                .map_err(net)?;
            let page: Value = crate::ensure_ok(response)
                .await?
                .json()
                .await
                .map_err(net)?;
            if let Some(blocks) = page["results"].as_array() {
                for block in blocks {
                    *visited += 1;
                    if *visited > 10_000 {
                        return Err(Error::BadRequest("Notion page exceeds 10000 blocks".into()));
                    }
                    markdown.push_str(&render_block(block));
                    if block["has_children"].as_bool().unwrap_or(false) {
                        if let Some(block_id) = block["id"].as_str() {
                            markdown.push_str(
                                &render_children(client, token, block_id, depth + 1, visited)
                                    .await?,
                            );
                        }
                    }
                }
            }
            cursor = page["next_cursor"].as_str().map(str::to_string);
            if !page["has_more"].as_bool().unwrap_or(false) || cursor.is_none() {
                break;
            }
        }
        Ok(markdown)
    })
}

#[async_trait]
impl Connector for Notion {
    fn provider(&self) -> &'static str {
        "notion"
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        let started_at = Utc::now();
        let previous = parse_cursor(ctx.cursor.as_deref());
        // Notion has no deletion feed. Use fast last-edited incremental scans most of the
        // time and force a complete reconciliation at least daily.
        let full_scan = previous
            .as_ref()
            .is_none_or(|cursor| started_at - cursor.last_full_at >= Duration::hours(24));
        let mut stats = SyncStats {
            reconcile_deletions: full_scan,
            ..Default::default()
        };
        let limit = ctx.document_limit.max(0);
        let mut start_cursor: Option<String> = None;
        let mut newest_edit = previous.as_ref().map(|cursor| cursor.last_edited);

        'pages: loop {
            let mut body = json!({
                "filter": {"property": "object", "value": "page"},
                "sort": {"direction": "descending", "timestamp": "last_edited_time"},
                "page_size": 100
            });
            if let Some(cursor) = &start_cursor {
                body["start_cursor"] = json!(cursor);
            }
            let resp = client
                .post("https://api.notion.com/v1/search")
                .bearer_auth(token)
                .header("Notion-Version", NOTION_VERSION)
                .json(&body)
                .send()
                .await
                .map_err(net)?;
            let search: Value = crate::ensure_ok(resp).await?.json().await.map_err(net)?;

            let empty = vec![];
            for page in search["results"].as_array().unwrap_or(&empty) {
                let edited_at = page["last_edited_time"]
                    .as_str()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|time| time.with_timezone(&Utc));
                if !full_scan
                    && edited_at.is_some_and(|edited| {
                        previous
                            .as_ref()
                            // Replay equal timestamps so two pages edited in the same
                            // provider clock tick cannot straddle the checkpoint and be lost.
                            .is_some_and(|cursor| edited < cursor.last_edited)
                    })
                {
                    break 'pages;
                }
                if let Some(edited) = edited_at {
                    newest_edit = Some(newest_edit.map_or(edited, |current| current.max(edited)));
                }
                let page_id = page["id"].as_str().unwrap_or_default();
                if page_id.is_empty() {
                    stats.failed += 1;
                    continue;
                }
                ctx.mark_seen(page_id);
                let title = page["properties"]
                    .as_object()
                    .and_then(|props| {
                        props.values().find_map(|p| {
                            if p["type"].as_str() == Some("title") {
                                Some(rich_text(&p["title"]))
                            } else {
                                None
                            }
                        })
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Notion page".to_string());

                let mut visited = 0;
                let md = match render_children(&client, token, page_id, 0, &mut visited).await {
                    Ok(markdown) => markdown,
                    Err(error) => {
                        tracing::warn!(page_id, %error, "failed to fetch Notion block tree");
                        stats.failed += 1;
                        continue;
                    }
                };
                if md.trim().is_empty() {
                    continue;
                }
                match ctx
                    .ingest(page_id, md, "notion_page", Some(title), None)
                    .await
                {
                    Ok(_) => stats.processed += 1,
                    Err(_) => stats.failed += 1,
                }
            }
            start_cursor = search["next_cursor"].as_str().map(str::to_string);
            if full_scan && limit > 0 && (stats.processed + stats.failed) >= limit {
                stats.truncated = true;
                break;
            }
            if !search["has_more"].as_bool().unwrap_or(false) || start_cursor.is_none() {
                break;
            }
        }
        if !stats.truncated {
            let cursor = NotionCursor {
                last_edited: newest_edit.unwrap_or(started_at),
                last_full_at: if full_scan {
                    started_at
                } else {
                    previous
                        .as_ref()
                        .map_or(started_at, |cursor| cursor.last_full_at)
                },
            };
            stats.cursor =
                Some(serde_json::to_string(&cursor).map_err(|error| {
                    Error::Internal(format!("serialize Notion cursor: {error}"))
                })?);
        }
        Ok(stats)
    }
}
