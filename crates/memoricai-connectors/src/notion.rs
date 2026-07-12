//! Notion connector: searches pages, renders block trees to markdown, ingests.
//! Notion webhooks aren't subscribed; the 4h cron polls (last-edited filter).

use crate::{http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use memoricai_core::error::Result;
use serde_json::{json, Value};

pub struct Notion;

const NOTION_VERSION: &str = "2022-06-28";

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

#[async_trait]
impl Connector for Notion {
    fn provider(&self) -> &'static str {
        "notion"
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        let mut stats = SyncStats::default();

        let resp = client
            .post("https://api.notion.com/v1/search")
            .bearer_auth(token)
            .header("Notion-Version", NOTION_VERSION)
            .json(&json!({"filter": {"property": "object", "value": "page"}, "page_size": 50}))
            .send()
            .await
            .map_err(net)?;
        let search: Value = crate::ensure_ok(resp).await?.json().await.map_err(net)?;

        let empty = vec![];
        for page in search["results"].as_array().unwrap_or(&empty) {
            let page_id = page["id"].as_str().unwrap_or_default();
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

            let blocks: Value = match client
                .get(format!(
                    "https://api.notion.com/v1/blocks/{page_id}/children"
                ))
                .bearer_auth(token)
                .header("Notion-Version", NOTION_VERSION)
                .query(&[("page_size", "100")])
                .send()
                .await
            {
                Ok(r) => r.json().await.unwrap_or(Value::Null),
                Err(_) => {
                    stats.failed += 1;
                    continue;
                }
            };
            let mut md = String::new();
            for b in blocks["results"].as_array().unwrap_or(&empty) {
                md.push_str(&render_block(b));
            }
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
        Ok(stats)
    }
}
