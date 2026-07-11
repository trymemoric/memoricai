//! Content-type detection and text extraction. Phase 1 handles text, markdown,
//! code, JSON/CSV (passthrough), URLs (fetch + readability-lite), and PDF bytes.

use futures::StreamExt;
use memoricai_core::enums::doc_type;
use memoricai_core::error::{Error, Result};
use scraper::{Html, Selector};
use url::Url;

pub struct Extracted {
    pub text: String,
    pub title: Option<String>,
}

/// Heuristic content-type detection from the raw content + optional title.
pub fn detect_type(content: &str, title: Option<&str>) -> String {
    let trimmed = content.trim();
    if is_url(trimmed) {
        if trimmed.contains("youtube.com") || trimmed.contains("youtu.be") {
            return doc_type::YOUTUBE.into();
        }
        if trimmed.contains("twitter.com") || trimmed.contains("x.com") {
            return doc_type::TWEET.into();
        }
        return doc_type::WEBPAGE.into();
    }
    let name = title.unwrap_or("");
    if name.ends_with(".md") || looks_like_markdown(trimmed) {
        return doc_type::MARKDOWN.into();
    }
    if name.ends_with(".json") || (trimmed.starts_with('{') && trimmed.ends_with('}')) {
        return doc_type::JSON.into();
    }
    if looks_like_code(trimmed) {
        return doc_type::CODE.into();
    }
    doc_type::TEXT.into()
}

pub fn is_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://")) && !s.contains(char::is_whitespace)
}

fn looks_like_markdown(s: &str) -> bool {
    s.lines().take(20).any(|l| {
        let l = l.trim_start();
        l.starts_with("# ") || l.starts_with("## ") || l.starts_with("- ") || l.starts_with("```")
    })
}

fn looks_like_code(s: &str) -> bool {
    let markers = [
        "fn ",
        "def ",
        "function ",
        "class ",
        "import ",
        "const ",
        "public ",
        "#include",
    ];
    let hits = s
        .lines()
        .take(40)
        .filter(|l| markers.iter().any(|m| l.trim_start().starts_with(m)))
        .count();
    hits >= 2
}

/// Extract plain text (and a title) from a document's raw content.
pub async fn extract(doc_type: &str, content: &str, url: Option<&str>) -> Result<Extracted> {
    match doc_type {
        "webpage" | "youtube" | "tweet" => {
            let target = url.unwrap_or(content).trim();
            if is_url(target) {
                fetch_and_extract(target).await
            } else {
                Ok(Extracted {
                    text: content.to_string(),
                    title: None,
                })
            }
        }
        _ => Ok(Extracted {
            text: content.to_string(),
            title: None,
        }),
    }
}

const MAX_FETCH_BYTES: usize = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

pub struct PublicFetch {
    pub final_url: Url,
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

/// Build an HTTP client pinned to the URL's currently resolved address. Private
/// addresses are rejected unless the caller has matched an explicit origin allowlist.
pub async fn validated_client(url: &Url, allow_private: bool) -> Result<reqwest::Client> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::BadRequest("URL must use http or https".into()));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(Error::BadRequest(
            "URL credentials and fragments are not allowed".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::BadRequest("URL is missing a host".into()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Error::BadRequest("URL is missing a port".into()))?;
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| Error::BadRequest("URL host could not be resolved".into()))?
        .collect();
    if addresses.is_empty()
        || (!allow_private
            && addresses
                .iter()
                .any(|address| memoricai_core::network::is_blocked_ip(address.ip())))
    {
        return Err(Error::Forbidden(
            "URL resolves to a non-public network address".into(),
        ));
    }
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("memoricai/0.1");
    if host.parse::<std::net::IpAddr>().is_err() {
        builder = builder.resolve(host, addresses[0]);
    }
    builder
        .build()
        .map_err(|error| Error::Internal(error.to_string()))
}

/// Fetch an untrusted public URL with DNS pinning, redirect revalidation, and a byte cap.
pub async fn fetch_public(url: &Url) -> Result<PublicFetch> {
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let client = validated_client(&current, false).await?;
        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|error| Error::BadRequest(format!("fetch failed: {error}")))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| Error::BadRequest("redirect is missing Location".into()))?;
            current = current
                .join(location)
                .map_err(|_| Error::BadRequest("invalid redirect URL".into()))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(Error::BadRequest(format!(
                "fetch {}: {}",
                current,
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_FETCH_BYTES as u64)
        {
            return Err(Error::BadRequest("fetched response is too large".into()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| Error::BadRequest(error.to_string()))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_FETCH_BYTES {
                return Err(Error::BadRequest("fetched response is too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(PublicFetch {
            final_url: current,
            bytes,
            content_type,
        });
    }
    Err(Error::BadRequest("too many redirects".into()))
}

async fn fetch_and_extract(url: &str) -> Result<Extracted> {
    let url = Url::parse(url).map_err(|_| Error::BadRequest("invalid URL".into()))?;
    let fetched = fetch_public(&url).await?;
    if fetched.content_type.as_deref().is_some_and(|content_type| {
        let media_type = content_type.split(';').next().unwrap_or("").trim();
        !matches!(
            media_type,
            "text/html" | "application/xhtml+xml" | "text/plain"
        )
    }) {
        return Err(Error::BadRequest(
            "URL did not return HTML or plain text".into(),
        ));
    }
    let html = String::from_utf8_lossy(&fetched.bytes);
    Ok(html_to_text(&html))
}

pub static BODY_SEL: std::sync::LazyLock<Selector> = std::sync::LazyLock::new(|| {
    Selector::parse("p, h1, h2, h3, h4, li, blockquote, pre, td").unwrap()
});

/// Very small readability: drop script/style/nav, collect visible text + <title>.
pub fn html_to_text(html: &str) -> Extracted {
    let doc = Html::parse_document(html);
    let title = Selector::parse("title")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty());

    let mut parts: Vec<String> = Vec::new();
    for el in doc.select(&BODY_SEL) {
        let t = el.text().collect::<String>();
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.len() >= 2 {
            parts.push(t);
        }
    }
    let text = if parts.is_empty() {
        // Fallback: strip tags crudely.
        let sel = Selector::parse("body").unwrap();
        doc.select(&sel)
            .next()
            .map(|b| b.text().collect::<String>())
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        parts.join("\n\n")
    };
    Extracted { text, title }
}

/// Extract text from PDF bytes (used by the file-upload endpoint).
pub fn extract_pdf_bytes(bytes: &[u8]) -> Result<String> {
    const MAX_PAGES: usize = 2_000;
    const MAX_PAGE_DECOMPRESSED_BYTES: usize = 4 * 1024 * 1024;
    let options = lopdf::LoadOptions::with_max_decompressed_size(MAX_DOCUMENT_PDF_BYTES);
    let document = lopdf::Document::load_mem_with_options(bytes, options)
        .map_err(|error| Error::BadRequest(format!("pdf: {error}")))?;
    let pages: Vec<u32> = document.get_pages().into_keys().collect();
    if pages.len() > MAX_PAGES {
        return Err(Error::BadRequest(format!(
            "PDF exceeds the {MAX_PAGES}-page limit"
        )));
    }
    let mut text = String::new();
    for page in pages {
        let page_text = document
            .extract_text_with_limit(&[page], MAX_PAGE_DECOMPRESSED_BYTES)
            .map_err(|error| Error::BadRequest(format!("pdf page {page}: {error}")))?;
        if text.len().saturating_add(page_text.len()) > MAX_DOCUMENT_PDF_BYTES {
            return Err(Error::BadRequest(
                "extracted PDF text exceeds 10 MiB".into(),
            ));
        }
        text.push_str(&page_text);
    }
    Ok(text)
}

const MAX_DOCUMENT_PDF_BYTES: usize = 10 * 1024 * 1024;
