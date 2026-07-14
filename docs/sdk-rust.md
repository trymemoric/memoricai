# Rust SDK (`memoricai-client`)

Async client for the [`/v1` HTTP API](api.md), built on `reqwest`. Request and
response types shared with the server are re-exported from `memoricai-core`;
route-specific client types use the same camelCase wire contract.

```toml
[dependencies]
memoricai-client = "0.3.2"
tokio = { version = "1", features = ["full"] }
```

## Quickstart

```rust
use memoricai_client::{Client, MemorySearchRequest};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), memoricai_client::ClientError> {
    let client = Client::new("http://localhost:7373", "mc_...");

    // Ingest returns instantly with status "queued"; processing is async.
    let doc = client.add_text("My name is Ada.", "mc_project_default").await?;
    client.wait_for_document(&doc.id, Duration::from_secs(60)).await?;

    let res = client
        .search_memories(&MemorySearchRequest {
            q: "what is my name".into(),
            container_tag: Some("mc_project_default".into()),
            digest: true,
            ..Default::default()
        })
        .await?;
    println!("{}", res.digest.unwrap_or_default());
    Ok(())
}
```

## Construction

```rust
let client = Client::new(base_url, api_key);
```

- `base_url`, e.g. `http://localhost:7373` (trailing slashes are trimmed).
- `api_key`, an `mc_...` organization or container-scoped key.
- Requests share a pooled HTTP client with a 120 s timeout. `Client` is `Clone`
  (cheap; the pool is shared).

## Errors

```rust
pub enum ClientError {
    Transport(reqwest::Error),          // network / timeout
    Api { status: u16, message: String }, // non-2xx; message from the error envelope
    ProcessingFailed(String, String),   // wait_for_document saw status "failed"
    Timeout(String),                    // wait_for_document deadline exceeded
}
```

The SDK retries transient failures (429/5xx) up to 4 times with exponential
backoff, matching the Python and TypeScript SDKs.

## Methods

### Documents

| Method | Endpoint |
|---|---|
| `add_document(&IngestRequest) -> IngestResponse` | `POST /v1/documents` |
| `add_text(content, container_tag) -> IngestResponse` | convenience wrapper |
| `get_document(id) -> Document` | `GET /v1/documents/{id}` |
| `delete_document(id) -> serde_json::Value` | `DELETE /v1/documents/{id}` |
| `list_documents(&DocumentListRequest) -> DocumentListResponse` | `POST /v1/documents/list` |
| `search_documents(&DocumentSearchRequest) -> DocumentSearchResponse` | `POST /v1/documents/search` |
| `wait_for_document(id, timeout) -> Document` | polls every 400 ms until `done` |

`wait_for_document` returns `ClientError::ProcessingFailed` if the pipeline
marks the document `failed`, and `ClientError::Timeout` past the deadline.

### Search & profile

| Method | Endpoint |
|---|---|
| `search_memories(&MemorySearchRequest) -> MemorySearchResponse` | `POST /v1/search` |
| `build_context(&ContextRequest) -> ContextResponse` | `POST /v1/context` |
| `profile(&ProfileRequest) -> ProfileResponse` | `POST /v1/profile` |

`MemorySearchRequest` implements `Default` with the server's defaults
(`search_mode: "hybrid"`, `limit: 10`, `threshold: 0.5`), so struct-update
syntax is the idiomatic call style. Set `digest: true` for the ready-to-inject
context block (`response.digest`).
Use `build_context` when the prompt needs a hard budget, coverage across multiple
sources, and structured inclusion/omission diagnostics.

### Memories

| Method | Endpoint |
|---|---|
| `create_memories(&CreateMemoriesRequest) -> CreateMemoriesResponse` | `POST /v1/memories` |
| `patch_memory(&PatchMemoryRequest) -> Memory` | `PATCH /v1/memories` |
| `forget_memory(&ForgetRequest) -> Memory` | `DELETE /v1/memories` |
| `forget_matching(&ForgetMatchingRequest) -> ForgetMatchingResponse` | `POST /v1/memories/forget-matching` |

### Misc

`health() -> serde_json::Value`, `GET /health` (no auth needed server-side,
but the SDK sends the key regardless).

The client also covers the complete v0.3.2 management surface: batch/multipart
ingestion; document patch, processing, and bulk operations; projects and
container tags; settings and scoped keys; profile buckets and inferred-memory
review; analytics; connectors; MCP helpers; provisioning; and the memory
router. `router_request` returns `reqwest::Response` so callers can consume SSE
streams directly. `request_json` is the forward-compatible escape hatch for
later engine endpoints.

## Integration test

The crate ships an ignored end-to-end test that exercises
ingest → wait → digest search → profile against a live server:

```bash
MEMORICAI_SDK_TEST_URL=http://localhost:7373 \
MEMORICAI_SDK_TEST_KEY=mc_... \
cargo test -p memoricai-client -- --ignored
```
