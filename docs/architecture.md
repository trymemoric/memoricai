# memoricai, Architecture

memoricai is a Rust memory & context engine compiled into one self-hostable binary. This
document describes how it is structured and how a request flows through it.

## Crate layout

The workspace has strictly downward dependencies (no cycles):

| Crate | Responsibility |
|---|---|
| `memoricai-core` | Pure domain: `Document`/`Memory`/`Chunk`/`Space`/`Profile`, enums, the `/v1` wire DTOs, the `MetadataFilter` AST, typed errors, and the provider **trait ports** (`LlmProvider`, `EmbeddingProvider`, `Reranker`, `Transcriber`, `Vision`). No I/O. |
| `memoricai-db` | sqlx repositories + SQL migrations over Postgres. pgvector is used through raw SQL casts (`$n::vector`, `<=>`), so the embedding dimension stays configurable and no pgvector crate is required. Only runtime queries, building never needs a live database. |
| `memoricai-models` | The pluggable model layer. `ModelStack` bundles an LLM, an embedder, a reranker, and optional transcriber/vision. Provider: an OpenAI-compatible client (covers OpenAI/Ollama/vLLM/LM Studio/…). Chat + embedding endpoints are required at startup; tests use a deterministic in-process fake (`ModelStack::for_tests`). |
| `memoricai-engine` | The ingestion pipeline, memory extraction, temporal graph, search, and profiles. `Engine` is the facade every higher layer builds on. |
| `memoricai-auth` | API-key minting/introspection (argon2-hashed, O(1) prefix lookup), container-scoped keys, a fixed-window rate limiter, tenant policy, and a full OAuth2/OIDC provider. |
| `memoricai-connectors` | The `Connector` trait, a sync engine with a SyncRun ledger, and 8 provider implementations. Depends on the engine to ingest fetched content. |
| `memoricai-api` | The axum HTTP surface for `/v1`, an auth extractor, JSON error shapes, OpenAPI, and the Memory Router proxy. |
| `memoricai-mcp` | A hand-rolled MCP Streamable-HTTP (JSON-RPC 2.0) server: tools, resources, a prompt, and `.well-known` discovery. |
| `memoricai` (bin) | Composition root: CLI (`serve`/`migrate`/`key create`), config from env, and background workers (ingest pool, forgetting sweeper, connector cron, profile-aggregation cron). |

## Request lifecycle

1. **Auth**, every request carries `Authorization: Bearer mc_...` (a full-org or
   container-scoped API key) or an OAuth2 access token. The API's `Auth` extractor introspects
   it, resolves the tenant (org + user + current membership + intersected token scope), and
   enforces read/write permission, organization role, endpoint capability, and container scope.
2. **Ingestion (accept-instantly)**, `POST /v1/documents` validates, stores a `queued`
   document, and returns `{id, status:"queued"}` in milliseconds. Postgres is the source of truth
   for the queue; a tokio worker pool atomically claims jobs with leases and bounded attempts.
   Abandoned jobs recover after restart. Searches never wait on the queue.
3. **Retrieval**, `/v1/documents/search` (chunk RAG), `/v1/search` (memory / hybrid), and `/v1/context` (bounded memory digest plus source-fair excerpts) embed the query
   (optionally rewriting it into variations), run vector search in the tenant's namespace, merge
   and threshold, optionally rerank, and attach version-graph context. `/v1/profile` serves a
   cached static/dynamic/bucket profile.

## Ingestion pipeline (engine)

`queued → extracting → chunking → embedding → indexing → done|failed`

1. **Content-type detection**, from URL patterns, extension, and structure.
2. **Extraction**, text/markdown/code pass through; HTML/URLs are fetched and reduced to main
   text; PDFs via a text extractor; images via a vision model (OCR + caption); audio/video via a
   transcriber. PDF parsing runs on a blocking worker. Untrusted URLs use DNS pinning, redirect
   revalidation, private-address denial, timeouts, content-type checks, and a 10 MiB response cap.
3. **Chunking**, markdown by heading, code by definition/blank-line boundaries, everything else
   by paragraph, then greedily packed to the organization's configured target size.
4. **Embedding**, batched, provider ordering/count/dimension/numeric output validated, then
   L2-normalized (so cosine == dot product).
5. **Memory extraction**, a constrained-JSON LLM call turns content into atomic facts
   (with `isStatic` and optional `forgetAfter`). Organization category/include/exclude/filter
   settings are applied to the extraction prompt and enforced after parsing.
6. **Relation inference + versioning**, each new fact is compared to its nearest neighbors;
   high similarity supersedes the old memory (a new version, predecessor marked not-latest,
   `updates` edge), moderate similarity adds an `extends` edge. A partial unique index enforces
   one latest memory per version-chain root.
7. **Forgetting**, `forgetAfter` timestamps are swept by a background job; forgotten memories
   are soft-deleted (excluded from search, retained for history).
8. **Profile building**, per-container `static` (identity facts), `dynamic` (recency-ranked),
   and topical `buckets`; older memories are periodically aggregated into `[Summary]` entries.

## Data model (Postgres)

Every content table is tenant-scoped by `org_id` + `space_container_tag`. Embeddings are stored
in `memory_embeddings` and `chunk_embeddings`, keyed to an `embedding_indexes` registry row that
records the organization, provider, model id, model version, and dimension. This keeps vector
spaces isolated while retained memory/chunk text remains model-independent. Vectors use
dimensionless pgvector columns; metadata uses `JSONB`; enums use text with fallbacks. Key
structures: `documents`, `memories` (+ `memory_relations` edge table, version-chain columns),
`chunks`, `embedding_indexes`/`embedding_backfill_jobs`, `spaces`,
`profile_buckets`/`profile_summaries`, identity tables
(`users`/`organizations`/`members`/`api_keys`), OAuth tables, `connections`/`sync_runs`, and an
`api_requests` analytics log.

Phase-appropriate simplification: vector search uses **exact scan**; add ANN (HNSW) indexes per
deployment once the embedding dimension is fixed.

## Auth & multi-tenancy

A user belongs to organizations via memberships (roles `owner > admin > member`; members can be
restricted to specific container tags). Data is partitioned by **container tags** (projects,
`mc_project_<name>`). API keys are opaque `mc_<orgId>_<random>` bearers, hashed at rest.
Container-scoped keys are limited to a fixed endpoint allowlist with their own rate limits.
Shared resources are readable when any attached container is authorized, but mutation requires
authorization for every attached container. The built-in OAuth2/OIDC provider supports the
authorization-code flow with mandatory PKCE S256 for public clients, atomically consumed codes,
rotating refresh tokens, dynamic client registration, `.well-known` discovery, and a scoped
token→API-key exchange used by the MCP server.

## Model layer

All model use goes through the `ModelStack` traits, so providers are swappable. Embeddings/LLM
use OpenAI-compatible endpoints and are required at startup; reranking, transcription, and
vision are optional and configured by env. Tests run against a deterministic in-process fake
(`ModelStack::for_tests`). Embeddings are always L2-normalized before storage. Query embeddings
are cached in-process (bounded, exact-match) so repeated searches skip the remote round-trip.
Changing the configured provider/model/version/dimension selects a distinct index and queues a
durable background re-embedding job from retained memory and chunk text.

## Background workers (binary)

- **Ingest pool**, atomically claims durable queued/retryable/abandoned jobs with bounded concurrency.
- **Embedding backfiller**, leases resumable batches for the configured model index and fills
  missing memory/chunk vectors from retained text.
- **Forgetting sweeper**, marks expired memories forgotten (every minute).
- **Connector cron**, runs due connector syncs (every 4 hours).
- **Profile-aggregation cron**, condenses old memories into `[Summary]` entries (every 6 hours).
