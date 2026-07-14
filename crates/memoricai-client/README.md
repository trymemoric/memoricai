# memoricai-client

Rust SDK for [memoricai](https://github.com/trymemoric/memoricai), a self-hostable
memory & context engine for AI agents (one Rust binary + Postgres/pgvector).

```rust
use memoricai_client::{Client, MemorySearchRequest};
use std::time::Duration;

# async fn demo() -> Result<(), memoricai_client::ClientError> {
let client = Client::new("http://localhost:7373", "mc_...");

let doc = client.add_text("My name is Ada.", "mc_project_default").await?;
client.wait_for_document(&doc.id, Duration::from_secs(60)).await?;

let res = client
    .search_memories(&MemorySearchRequest {
        q: "what is my name".into(),
        container_tag: Some("mc_project_default".into()),
        digest: true, // ready-to-inject, date-stamped context
        ..Default::default()
    })
    .await?;
println!("{}", res.digest.unwrap_or_default());
# Ok(())
# }
```

Covers the full v0.3.2 `/v1` API: batch/file ingestion and document lifecycle,
search/context/profile, direct memory management, projects, settings, scoped
keys, analytics, connectors, buckets, inferred-memory review, MCP helpers,
provisioning, and the memory router. Router responses remain raw so callers can
consume streamed SSE bodies. Shared request/response types are re-exported from
`memoricai-core`; `request_json` is available for forward-compatible access to
new endpoints.
