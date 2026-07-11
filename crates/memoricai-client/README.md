# memoricai-client

Rust SDK for [memoricai](https://github.com/skundu42/memoricai) — a self-hostable
memory & context engine for AI agents (one Rust binary + Postgres/pgvector).

```rust
use memoricai_client::{Client, MemorySearchRequest};
use std::time::Duration;

# async fn demo() -> Result<(), memoricai_client::ClientError> {
let client = Client::new("http://localhost:6767", "mc_...");

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

Covers the full `/v1` API: documents (add/get/list/delete/search), memory-graph
search with the `digest` context mode, profiles, and direct memory management
(create/patch/forget/bulk-forget). Request/response types are re-exported from
`memoricai-core` — the same definitions the server compiles against.
