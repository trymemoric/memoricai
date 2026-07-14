//! Web-crawler connector: same-domain BFS with an SSRF guard. No auth, no
//! webhooks — recrawls on the cron schedule.

use crate::{Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use memoricai_core::error::{Error, Result};
use memoricai_engine::extract::BODY_SEL;
use scraper::{Html, Selector};
use std::collections::{HashSet, VecDeque};
use url::Url;

pub struct WebCrawler;

const MAX_PAGES: usize = 25;
const MAX_DEPTH: usize = 2;
/// A single page can link to an unbounded number of same-host URLs; cap how many we
/// harvest per page and how large the frontier can grow so a hostile or pathological
/// site cannot exhaust memory during a crawl.
const MAX_LINKS_PER_PAGE: usize = 100;
const MAX_QUEUE: usize = MAX_PAGES * 100;

static LINK_SEL: std::sync::LazyLock<Selector> =
    std::sync::LazyLock::new(|| Selector::parse("a[href]").unwrap());

fn extract(html: &str, base: &Url) -> (String, Vec<Url>) {
    let doc = Html::parse_document(html);
    let text = doc
        .select(&BODY_SEL)
        .map(|el| {
            el.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|t| t.len() >= 2)
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut links = Vec::new();
    for a in doc.select(&LINK_SEL) {
        if links.len() >= MAX_LINKS_PER_PAGE {
            break;
        }
        if let Some(href) = a.value().attr("href") {
            if let Ok(u) = base.join(href) {
                if u.host_str() == base.host_str() {
                    links.push(u);
                }
            }
        }
    }
    (text, links)
}

#[async_trait]
impl Connector for WebCrawler {
    fn provider(&self) -> &'static str {
        "web-crawler"
    }
    fn is_oauth(&self) -> bool {
        false
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let start = ctx.metadata["startUrl"]
            .as_str()
            .ok_or_else(|| Error::BadRequest("web-crawler requires metadata.startUrl".into()))?;
        let start_url =
            Url::parse(start).map_err(|_| Error::BadRequest("invalid startUrl".into()))?;
        let start_host = start_url.host_str().map(str::to_string);
        let mut stats = SyncStats::default();

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(Url, usize)> = VecDeque::new();
        queue.push_back((start_url, 0));

        while let Some((url, depth)) = queue.pop_front() {
            if stats.processed as usize >= MAX_PAGES || visited.len() >= MAX_PAGES * 2 {
                break;
            }
            if !visited.insert(url.as_str().to_string()) {
                continue;
            }

            let fetched = match memoricai_engine::extract::fetch_public(&url).await {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!(url = %url, %error, "web-crawler fetch rejected");
                    stats.failed += 1;
                    continue;
                }
            };
            if fetched.final_url.host_str() != start_host.as_deref() {
                tracing::warn!(url = %url, final_url = %fetched.final_url, "cross-domain redirect rejected");
                stats.failed += 1;
                continue;
            }
            if !fetched
                .content_type
                .as_deref()
                .map(|content_type| content_type.contains("text/html"))
                .unwrap_or(true)
            {
                continue;
            }
            // HTML parsing is CPU-bound; keep it off the async runtime worker threads.
            let html = String::from_utf8_lossy(&fetched.bytes).into_owned();
            let base = fetched.final_url.clone();
            let (text, links) = match tokio::task::spawn_blocking(move || extract(&html, &base)).await
            {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(url = %url, %error, "web-crawler html parse failed");
                    stats.failed += 1;
                    continue;
                }
            };
            if !text.trim().is_empty() {
                if ctx
                    .ingest(
                        fetched.final_url.as_str(),
                        text,
                        "webpage",
                        Some(fetched.final_url.to_string()),
                        None,
                    )
                    .await
                    .is_ok()
                {
                    stats.processed += 1;
                } else {
                    stats.failed += 1;
                }
            }
            if depth < MAX_DEPTH {
                for l in links {
                    if queue.len() >= MAX_QUEUE {
                        break;
                    }
                    queue.push_back((l, depth + 1));
                }
            }
        }
        Ok(stats)
    }
}
