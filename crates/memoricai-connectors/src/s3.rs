//! S3 / S3-compatible connector: hand-rolled AWS SigV4 for ListObjectsV2 +
//! GetObject. No OAuth; cron-polled. Supports MinIO/R2/Spaces via `endpoint`.

use crate::{hex, net, Connector, ImportCtx, SyncStats};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use memoricai_core::error::{Error, Result};
use regex::Regex;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

async fn response_text_limited(response: reqwest::Response, max_bytes: usize) -> Result<String> {
    if !response.status().is_success() {
        return Err(Error::Model(format!(
            "S3 request failed: {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(Error::BadRequest("S3 response is too large".into()));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(net)?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(Error::BadRequest("S3 response is too large".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| Error::BadRequest("S3 object is not UTF-8".into()))
}

pub struct S3;

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// URI-encode a path, preserving `/`.
fn uri_encode_path(p: &str) -> String {
    p.split('/')
        .map(crate::oauth::urlencode)
        .collect::<Vec<_>>()
        .join("/")
}

struct S3Config {
    access_key: String,
    secret_key: String,
    bucket: String,
    region: String,
    host: String,
    scheme: String,
    origin: url::Url,
    allow_private: bool,
}

impl S3 {
    fn config(ctx: &ImportCtx<'_>) -> Result<S3Config> {
        let m = &ctx.metadata;
        let access_key = m["accessKeyId"]
            .as_str()
            .ok_or_else(|| Error::BadRequest("s3 accessKeyId required".into()))?
            .to_string();
        let secret_key = m["secretAccessKey"]
            .as_str()
            .ok_or_else(|| Error::BadRequest("s3 secretAccessKey required".into()))?
            .to_string();
        let bucket = m["bucket"]
            .as_str()
            .ok_or_else(|| Error::BadRequest("s3 bucket required".into()))?
            .to_string();
        let region = m["region"].as_str().unwrap_or("us-east-1").to_string();
        let endpoint = m["endpoint"]
            .as_str()
            .map(|endpoint| {
                if endpoint.contains("://") {
                    endpoint.to_string()
                } else {
                    format!("https://{endpoint}")
                }
            })
            .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        let origin = url::Url::parse(&endpoint)
            .map_err(|_| Error::BadRequest("invalid S3 endpoint".into()))?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.username() != ""
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(Error::BadRequest(
                "S3 endpoint must be an HTTP(S) origin without credentials, path, query, or fragment"
                    .into(),
            ));
        }
        let port = origin
            .port_or_known_default()
            .ok_or_else(|| Error::BadRequest("S3 endpoint has no port".into()))?;
        let normalized_origin = format!(
            "{}://{}:{}",
            origin.scheme(),
            origin
                .host_str()
                .ok_or_else(|| Error::BadRequest("S3 endpoint has no host".into()))?,
            port
        );
        let allow_private = std::env::var("MEMORICAI_CONNECTOR_ALLOWED_ORIGINS")
            .ok()
            .is_some_and(|origins| {
                origins.split(',').map(str::trim).any(|allowed| {
                    url::Url::parse(allowed).ok().is_some_and(|allowed| {
                        allowed.host_str().is_some_and(|host| {
                            format!(
                                "{}://{}:{}",
                                allowed.scheme(),
                                host,
                                allowed.port_or_known_default().unwrap_or(0)
                            ) == normalized_origin
                        })
                    })
                })
            });
        if origin.scheme() != "https" && !allow_private {
            return Err(Error::Forbidden(
                "S3 endpoints must use HTTPS unless their origin is explicitly allowlisted".into(),
            ));
        }
        let scheme = origin.scheme().to_string();
        let host_name = match origin.host() {
            Some(url::Host::Ipv6(address)) => format!("[{address}]"),
            Some(host) => host.to_string(),
            None => return Err(Error::BadRequest("S3 endpoint has no host".into())),
        };
        let host = if origin.port().is_some() {
            format!("{host_name}:{port}")
        } else {
            host_name
        };
        Ok(S3Config {
            access_key,
            secret_key,
            bucket,
            region,
            host,
            scheme,
            origin,
            allow_private,
        })
    }

    /// Build a signed GET url + Authorization header for a canonical path + query.
    fn signed_get(
        cfg: &S3Config,
        canonical_path: &str,
        query: &str,
    ) -> (String, String, String, String) {
        let now = Utc::now();
        let amzdate = now.format("%Y%m%dT%H%M%SZ").to_string();
        let datestamp = now.format("%Y%m%d").to_string();
        let payload_hash = sha256_hex(b"");

        let canonical_uri = uri_encode_path(canonical_path);
        let canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            cfg.host, payload_hash, amzdate
        );
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_request = format!(
            "GET\n{canonical_uri}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );

        let scope = format!("{datestamp}/{}/s3/aws4_request", cfg.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amzdate}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );

        let k_date = hmac(
            format!("AWS4{}", cfg.secret_key).as_bytes(),
            datestamp.as_bytes(),
        );
        let k_region = hmac(&k_date, cfg.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            cfg.access_key
        );
        let url = if query.is_empty() {
            format!("{}://{}{}", cfg.scheme, cfg.host, canonical_uri)
        } else {
            format!("{}://{}{}?{}", cfg.scheme, cfg.host, canonical_uri, query)
        };
        (url, auth, amzdate, payload_hash)
    }
}

#[async_trait]
impl Connector for S3 {
    fn provider(&self) -> &'static str {
        "s3"
    }
    fn is_oauth(&self) -> bool {
        false
    }

    async fn import(&self, ctx: &ImportCtx<'_>) -> Result<SyncStats> {
        let cfg = Self::config(ctx)?;
        let client =
            memoricai_engine::extract::validated_client(&cfg.origin, cfg.allow_private).await?;
        let mut stats = SyncStats::default();

        let prefix = ctx.metadata["prefix"].as_str().unwrap_or("");
        let tag_regex = ctx.metadata["containerTagRegex"]
            .as_str()
            .and_then(|r| Regex::new(r).ok());

        // ListObjectsV2 (path-style).
        let list_path = format!("/{}", cfg.bucket);
        let query = if prefix.is_empty() {
            "list-type=2".to_string()
        } else {
            format!("list-type=2&prefix={}", crate::oauth::urlencode(prefix))
        };
        let (url, auth, amzdate, payload_hash) = Self::signed_get(&cfg, &list_path, &query);
        let resp = client
            .get(url)
            .header("Authorization", auth)
            .header("x-amz-date", amzdate)
            .header("x-amz-content-sha256", payload_hash)
            .send()
            .await
            .map_err(net)?;
        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Unauthorized(
                "s3 access denied (check keys/region)".into(),
            ));
        }
        let xml = response_text_limited(resp, 2 * 1024 * 1024).await?;

        static KEY_RE: std::sync::LazyLock<Regex> =
            std::sync::LazyLock::new(|| Regex::new(r"<Key>([^<]+)</Key>").unwrap());
        let mut count = 0;
        for cap in KEY_RE.captures_iter(&xml) {
            if count >= 200 {
                break;
            }
            let key = cap[1].to_string();
            if key.ends_with('/') {
                continue;
            }
            // Only fetch text-ish objects.
            let lower = key.to_lowercase();
            if !(lower.ends_with(".txt")
                || lower.ends_with(".md")
                || lower.ends_with(".json")
                || lower.ends_with(".csv")
                || lower.ends_with(".log")
                || lower.ends_with(".html"))
            {
                continue;
            }
            count += 1;

            let obj_path = format!("/{}/{}", cfg.bucket, key);
            let (ourl, oauth_, odate, ohash) = Self::signed_get(&cfg, &obj_path, "");
            let content = match client
                .get(ourl)
                .header("Authorization", oauth_)
                .header("x-amz-date", odate)
                .header("x-amz-content-sha256", ohash)
                .send()
                .await
            {
                Ok(response) => {
                    match response_text_limited(response, memoricai_engine::MAX_DOCUMENT_BYTES)
                        .await
                    {
                        Ok(content) => content,
                        Err(_) => {
                            stats.failed += 1;
                            continue;
                        }
                    }
                }
                Err(_) => {
                    stats.failed += 1;
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }
            // Optional per-object container tag from a named capture.
            let tags = tag_regex.as_ref().and_then(|re| {
                re.captures(&key).and_then(|c| {
                    c.name("userId")
                        .map(|m| vec![format!("mc_project_{}", m.as_str())])
                })
            });
            match ctx
                .ingest(&key, content, "text", Some(key.clone()), tags)
                .await
            {
                Ok(_) => stats.processed += 1,
                Err(_) => stats.failed += 1,
            }
        }
        Ok(stats)
    }
}
