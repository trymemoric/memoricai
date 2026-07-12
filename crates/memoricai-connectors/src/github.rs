//! GitHub connector: two-step (list repos -> configure) with real markdown
//! import and HMAC-verified push webhooks.

use crate::{hex, http, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use memoricai_core::error::{Error, Result};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;

type HmacSha256 = Hmac<Sha256>;

pub struct GitHub;

const DOC_EXTS: &[&str] = &[".md", ".mdx", ".markdown", ".txt", ".rst", ".adoc", ".org"];

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn gh_get(client: &reqwest::Client, token: &str, url: &str) -> Result<Value> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(net)?;
    crate::ensure_ok(resp).await?.json().await.map_err(net)
}

impl GitHub {
    fn selected_repos(ctx: &ImportCtx<'_>) -> Option<Vec<String>> {
        ctx.cursor
            .as_deref()
            .and_then(|c| serde_json::from_str::<Vec<String>>(c).ok())
            .filter(|v| !v.is_empty())
    }
}

#[async_trait]
impl Connector for GitHub {
    fn provider(&self) -> &'static str {
        "github"
    }

    async fn resources(&self, ctx: &ImportCtx<'_>, page: u32, per_page: u32) -> Result<Value> {
        let token = ctx.token()?;
        let client = http();
        let url = format!(
            "https://api.github.com/user/repos?per_page={}&page={}&sort=updated",
            per_page.clamp(1, 100),
            page.max(1)
        );
        let v = gh_get(&client, token, &url).await?;
        let repos: Vec<Value> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| {
                        json!({
                            "id": r["id"],
                            "name": r["name"],
                            "full_name": r["full_name"],
                            "description": r["description"],
                            "private": r["private"],
                            "default_branch": r["default_branch"],
                            "updated_at": r["updated_at"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"resources": repos, "total_count": repos.len()}))
    }

    async fn configure(&self, ctx: &ImportCtx<'_>, resources: Value) -> Result<Value> {
        let token = ctx.token()?;
        let client = http();
        // Persist selected repos (full_name) in the connection cursor.
        let repos: Vec<String> = resources["resources"]
            .as_array()
            .or_else(|| resources.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        r["full_name"]
                            .as_str()
                            .or_else(|| r["name"].as_str())
                            .map(|s| s.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        ctx.engine
            .db
            .set_connection_synced(
                &ctx.connection_id,
                Some(&serde_json::to_string(&repos).unwrap_or_default()),
            )
            .await
            .ok();

        // Best-effort webhook registration.
        let mut registered = 0;
        if let Ok(secret) = std::env::var("MEMORICAI_GITHUB_WEBHOOK_SECRET") {
            let hook_url = format!(
                "{}/v1/connections/webhooks/github",
                crate::oauth::base_url()
            );
            let body = json!({
                "name": "web",
                "active": true,
                "events": ["push", "delete"],
                "config": {"url": hook_url, "content_type": "json", "secret": secret}
            });
            for full in &repos {
                let ok = client
                    .post(format!("https://api.github.com/repos/{full}/hooks"))
                    .bearer_auth(token)
                    .header("Accept", "application/vnd.github+json")
                    .json(&body)
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                if ok {
                    registered += 1;
                } else {
                    tracing::info!(repo = %full, "github webhook not registered (falling back to cron polling)");
                }
            }
        } else {
            tracing::info!(
                "MEMORICAI_GITHUB_WEBHOOK_SECRET unset — github sync relies on cron polling"
            );
        }

        Ok(json!({"success": true, "message": "configured", "webhooksRegistered": registered}))
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let token = ctx.token()?;
        let client = http();
        let mut stats = SyncStats::default();

        // Determine repos: configured selection, else the first page of user repos.
        let repos: Vec<(String, String)> = match Self::selected_repos(ctx) {
            Some(full_names) => {
                let mut out = Vec::new();
                for full in full_names {
                    if let Ok(r) = gh_get(
                        &client,
                        token,
                        &format!("https://api.github.com/repos/{full}"),
                    )
                    .await
                    {
                        let branch = r["default_branch"].as_str().unwrap_or("main").to_string();
                        out.push((full, branch));
                    }
                }
                out
            }
            None => {
                let v = gh_get(
                    &client,
                    token,
                    "https://api.github.com/user/repos?per_page=20&sort=updated",
                )
                .await?;
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|r| {
                                Some((
                                    r["full_name"].as_str()?.to_string(),
                                    r["default_branch"].as_str().unwrap_or("main").to_string(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
        };

        for (full, branch) in repos {
            let tree = match gh_get(
                &client,
                token,
                &format!("https://api.github.com/repos/{full}/git/trees/{branch}?recursive=1"),
            )
            .await
            {
                Ok(t) => t,
                Err(_) => {
                    stats.failed += 1;
                    continue;
                }
            };
            let empty = vec![];
            let mut fetched = 0;
            for node in tree["tree"].as_array().unwrap_or(&empty) {
                if node["type"].as_str() != Some("blob") {
                    continue;
                }
                let path = node["path"].as_str().unwrap_or_default();
                let lower = path.to_lowercase();
                if !DOC_EXTS.iter().any(|e| lower.ends_with(e)) {
                    continue;
                }
                if fetched >= 100 {
                    break;
                }
                let file = gh_get(
                    &client,
                    token,
                    &format!("https://api.github.com/repos/{full}/contents/{path}?ref={branch}"),
                )
                .await;
                let content = match file {
                    Ok(f) => f["content"]
                        .as_str()
                        .and_then(|c| {
                            base64::engine::general_purpose::STANDARD
                                .decode(c.replace('\n', ""))
                                .ok()
                        })
                        .and_then(|b| String::from_utf8(b).ok())
                        .unwrap_or_default(),
                    Err(_) => {
                        stats.failed += 1;
                        continue;
                    }
                };
                if content.trim().is_empty() {
                    continue;
                }
                fetched += 1;
                match ctx
                    .ingest(
                        &format!("{full}/{path}"),
                        content,
                        "github_markdown",
                        Some(format!("{full}/{path}")),
                        None,
                    )
                    .await
                {
                    Ok(_) => stats.processed += 1,
                    Err(_) => stats.failed += 1,
                }
            }
        }
        Ok(stats)
    }

    async fn handle_webhook(
        &self,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Result<Option<String>> {
        let secret = std::env::var("MEMORICAI_GITHUB_WEBHOOK_SECRET")
            .map_err(|_| Error::BadRequest("webhook secret not configured".into()))?;
        let sig = headers
            .get("x-hub-signature-256")
            .or_else(|| headers.get("X-Hub-Signature-256"))
            .ok_or_else(|| Error::Unauthorized("missing signature".into()))?;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|_| Error::Internal("bad hmac key".into()))?;
        mac.update(body);
        let expected = format!("sha256={}", hex(&mac.finalize().into_bytes()));
        if !constant_time_eq(expected.as_bytes(), sig.as_bytes()) {
            return Err(Error::Unauthorized("invalid webhook signature".into()));
        }
        // Scope the sync to the repository this event is about, so a single repo's push
        // does not re-sync every GitHub connection in the deployment. Events without a
        // repository (e.g. ping) trigger no sync.
        let repo = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v["repository"]["full_name"].as_str().map(|s| s.to_string()));
        Ok(repo)
    }
}
