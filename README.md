<div align="center">

<img src="assets/memoricai-m-logo.svg" alt="memoricai logo" width="96" height="96">

# memoricai

**The second brain for AI agents, a self-hostable memory & context engine in a single Rust binary.**

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](rust-toolchain.toml)


[Benchmarks](#benchmarks) •
[Quickstart](#quickstart) •
[Configuration](#configuration) •
[HTTP API](#http-api) •
[MCP server](#mcp-server) •
[Connectors](#connectors) •
[Development](#development)

</div>

---

A context window is not a memory. Agents need more than a bigger prompt, they need durable facts, recall that tracks what changed, and a compact context packet assembled for the task at hand.

memoricai is that memory layer, the second brain for your agents. It turns raw content, documents, conversations, tool results, and decisions (text, Markdown, code, URLs, PDFs, images, audio/video), into a **temporal graph of atomic memories linked to their sources**, with version chains, relations, and automatic forgetting, then serves it back through hybrid semantic search, auto-maintained profiles, and ready-to-inject context digests, so every agent can pick up where the last run left off.

Everything ships in a single binary: HTTP API (`/v1`), an [MCP](https://modelcontextprotocol.io) server, an OAuth2/OIDC provider, eight data connectors, and a memory-injecting LLM proxy. One memory layer, any agent, run it fully managed at [memoric.xyz](https://memoric.xyz) or self-host it. The only hard dependency is **Postgres with pgvector**; the model layer speaks to any OpenAI-compatible provider (OpenAI, Ollama, LM Studio, vLLM) configured with two environment variables.

> **Status:** pre-1.0. APIs may change between minor versions.

## Features

- **Ingestion pipeline**, turns text, URLs, documents, code, images, and audio/video into embedded atomic memories using durable Postgres-backed jobs with retries, versioning, relations, and expiry.
- **Search**, combines chunk-level RAG with graph-aware memory search across `memories`, `hybrid`, and `documents` modes, with optional query rewriting, reranking, and ready-to-inject context digests.
- **Profiles**, builds static, dynamic, and bucketed user profiles in the background and serves them through a fast lookup path.
- **Multi-tenancy & auth**, isolates organizations with API keys, rate-limited scoped keys, and a built-in OAuth2/OIDC provider for MCP clients.
- **Connectors**, syncs Google Drive, Gmail, Notion, OneDrive, GitHub, Granola, S3-compatible stores, and guarded web crawls with per-run tracking.
- **Memory Router**, injects relevant memories into OpenAI-compatible chat requests before forwarding them to the upstream model.

## Benchmarks

Both major long-term-memory benchmarks, run against this codebase with commodity models (`gpt-4o-mini` extraction, `text-embedding-3-small` embeddings) on 2026-07-11. Answer contexts combine the `/v1/search` memory digest (`digest: true`) with the top retrieved sessions. Vendor numbers are their published results, not reruns; treat cross-system comparisons accordingly. Reproduce with [`scripts/longmemeval.py`](scripts/longmemeval.py) and [`scripts/locomo.py`](scripts/locomo.py).

### LongMemEval-S

Full 500-question set ([cleaned 2025-09 release](https://github.com/xiaowu0162/longmemeval)), official prompts, `gpt-4o-2024-08-06` as both answering model and judge, ~11k-token contexts.

| Category | memoricai | Supermemory¹ | Zep² | Full-context GPT-4o² |
|---|---|---|---|---|
| Single-session (assistant) | **100%** | 100% | 80.4% | 94.6% |
| Single-session (user) | **97.1%** | 97% | 92.9% | 81.4% |
| Temporal reasoning | 89.5% | 91% | 62.4% | 45.1% |
| Knowledge update | 87.2% | 99% | 83.3% | 78.2% |
| Multi-session | 74.4% | 93% | 57.9% | 44.3% |
| Single-session (preference) | 63.3% | 90% | 56.7% | 20.0% |
| **Overall** | **85.8%** ± 3.1 | 81.6-95%¹ | 71.2% | 60.2% |

Session-level retrieval across the 500 questions: **99.2% Recall@5, 100% Recall@10, 99.4% all-evidence coverage@10** (Supermemory reports 95% Recall@15). The abstention subset (n=30) scored 70%. Ingesting the benchmark's ~60M tokens of chat history (23,867 documents) completed with zero failed documents at 48 ingest workers.

### LoCoMo

All 10 conversations of [LoCoMo](https://github.com/snap-research/locomo), evaluated with [Mem0's published protocol](https://arxiv.org/abs/2504.19413): `gpt-4o-mini` answering model, LLM-as-a-judge, adversarial category excluded (n=1,540).

| Category | memoricai | Mem0³ | Mem0-graph³ | Zep³ | Full-context³ |
|---|---|---|---|---|---|
| Single-hop | **81.3%** | 67.1% | 65.7% | 61.7% | n/a |
| Temporal | **71.7%** | 55.5% | 58.1% | 49.3% | n/a |
| Multi-hop | **52.5%** | 51.2% | 47.2% | 41.4% | n/a |
| Open-domain | 51.0% | 72.9% | 75.7% | 76.6% | n/a |
| **Overall** | **72.1%** | 66.9% | 68.4% | 66.0% | 72.9% |

On the adversarial category (446 unanswerable questions, excluded above per Mem0's protocol), memoricai abstains correctly on 76.7% with its default answering prompt. Session-level retrieval: 79.0% Recall@5, 90.5% Recall@10.

### Engine performance

11-core Apple Silicon laptop, local Postgres: ~20 ms search latency at the engine+DB floor, 34-58 ms end-to-end for repeated queries against a hosted embedding provider (query-embedding cache), ~250 searches/s at 16-way concurrency, and ~290 MB RSS under full ingest load. Novel queries additionally pay one embedding-provider round-trip.

¹ Self-reported. 81.6% is [Supermemory's published GPT-4o-reader result](https://supermemory.ai/research/longmembench/), the like-for-like comparison; their per-category figures and 95% overall come from the same page's more favorable configuration.
² As reported in Zep's LongMemEval paper (GPT-4o reader).
³ LLM-judge scores from Mem0's paper (their measurements, including their Zep run, which Zep disputes; Zep claims ~75% under its own setup).

## Quickstart

Prerequisites: **Rust 1.88+** and **Postgres with the pgvector extension**.

```bash
# 1. Postgres + pgvector, pick one:
docker run -d --name memoricai-pg -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=memoricai -p 5432:5432 pgvector/pgvector:pg16
# ...or with Homebrew:
#   brew install postgresql@16 pgvector && brew services start postgresql@16
#   createdb memoricai && psql -d memoricai -c 'CREATE EXTENSION IF NOT EXISTS vector;'

# 2. Build (or: docker pull ghcr.io/trymemoric/memoricai:latest)
cargo build --release

# 3. Point it at your models (see docs/configuration.md)
export MEMORICAI_LLM_BASE_URL=https://api.openai.com/v1
export MEMORICAI_EMBEDDING_BASE_URL=https://api.openai.com/v1
export OPENAI_API_KEY=sk-...

# 4. Configure production secrets and create the first owner key
export MEMORICAI_DATABASE_URL="postgres://postgres:postgres@localhost:5432/memoricai"
export MEMORICAI_ENV=production
export MEMORICAI_ENCRYPTION_KEY="$(openssl rand -base64 32)"
./target/release/memoricai key create --org-name myorg --email me@example.com

# 5. Run (migrations apply automatically on startup)
./target/release/memoricai serve
```

Production never creates or prints an owner credential implicitly. Create keys through the explicit command above and store the encryption key and printed API key in your secret manager. Debug/development builds retain the first-run bootstrap convenience. You can mint additional organizations at any time:

```bash
./target/release/memoricai key create --org-name myorg --email me@example.com
```

Talk to it:

```bash
KEY=mc_...   # the printed key

# ingest
curl -s localhost:7373/v1/documents \
  -H "Authorization: Bearer $KEY" -H 'content-type: application/json' \
  -d '{"content":"My name is Ada and I love Rust.","containerTag":"mc_project_default"}'

# search (hybrid memory + chunk search)
curl -s localhost:7373/v1/search \
  -H "Authorization: Bearer $KEY" -H 'content-type: application/json' \
  -d '{"q":"what is my name","containerTag":"mc_project_default","searchMode":"hybrid"}'

# profile
curl -s localhost:7373/v1/profile \
  -H "Authorization: Bearer $KEY" -H 'content-type: application/json' \
  -d '{"containerTag":"mc_project_default"}'
```

## Configuration

See [Configuration](docs/configuration.md) for model setup examples and the complete environment-variable reference.

## HTTP API

Authenticate with `Authorization: Bearer mc_...`. Errors return a consistent JSON shape with appropriate status codes. A minimal OpenAPI 3.1 discovery stub is served at `GET /v1/openapi`; `GET /health` is unauthenticated.

JSON and multipart bodies are capped at 12 MiB; individual document/file content is capped at 10 MiB. Uploaded files are extracted in memory and are not exposed through a public filesystem route. URL ingestion and crawler redirects are DNS-resolved, pinned, size-limited, and blocked from private, loopback, link-local, and cloud-metadata ranges.

| Area | Endpoints |
|---|---|
| Documents | `POST/GET /v1/documents`, `/v1/documents/{batch,file,list,documents,processing,bulk}`, `GET/PATCH/DELETE /v1/documents/{id}` |
| Memories | `POST/PATCH/DELETE /v1/memories`, `POST /v1/memories/forget-matching` (bulk semantic forget with dry-run) |
| Search | `POST/GET /v1/documents/search` (chunk RAG), `POST /v1/search` (`searchMode`: `memories` \| `hybrid` \| `documents`; `digest: true` adds a compact date-stamped context digest) |
| Profiles | `POST /v1/profile`, `POST /v1/profile/buckets`, `POST /v1/buckets` |
| Projects | `GET/POST /v1/projects`, `DELETE /v1/projects/{id}`, `/v1/container-tags/*` (incl. inferred-memory review) |
| Auth | `GET /v1/session`, `POST /v1/auth/scoped-key`, `DELETE /v1/auth/scoped-key/{id}` |
| OAuth2/OIDC | `/api/auth/oauth2/{authorize,consent,token,register}`, `/.well-known/{oauth-authorization-server,openid-configuration}` |
| Connections | `GET/POST /v1/connections`, `/v1/connections/{id}/{import,sync-runs,resources,configure}`, OAuth callbacks, webhooks |
| Analytics | `GET /v1/analytics/{usage,errors,logs,memory,chat}` |
| Settings | `GET/PATCH /v1/settings`, `POST /v1/settings/reset` |
| Memory Router | `POST /v1/router/{*target}`, OpenAI-compatible proxy; `Authorization` carries the upstream key, `x-memoricai-api-key` the memoricai key, optional `x-mc-project` selects the project |

See [`docs/api.md`](docs/api.md) for the full endpoint reference (request/response fields, error envelope, metadata filters), and [`docs/architecture.md`](docs/architecture.md) for request lifecycles, the data model, and design rationale.

## SDKs

First-party clients for the `/v1` API, all covering documents, search (including the `digest` context mode), profiles, and memory management, with 429/5xx retry built in:

| Language | Where | Install |
|---|---|---|
| Rust | [`crates/memoricai-client`](crates/memoricai-client) | `cargo add memoricai-client` |
| Python (3.9+, stdlib-only) | [`sdks/python`](sdks/python) | `pip install memoricai` |
| TypeScript (Node 18+/Bun/Deno, zero-dep) | [`sdks/typescript`](sdks/typescript) | `npm install @memoricai/sdk` |

Detailed guides: [Rust](docs/sdk-rust.md) · [Python](docs/sdk-python.md) · [TypeScript](docs/sdk-typescript.md). All three follow the same shape, construct a client with base URL + `mc_` key, add content, wait for processing, then search with `digest: true` for ready-to-inject context:

```python
from memoricai import Client
client = Client("http://localhost:7373", "mc_...")
doc = client.add_text("My name is Ada.", container_tag="mc_project_default")
client.wait_for_document(doc["id"])
print(client.search_memories("what is my name",
                             container_tag="mc_project_default", digest=True)["digest"])
```

## MCP server

The binary serves MCP (Streamable HTTP, JSON-RPC over `POST /mcp`) alongside the REST API. It accepts either an `mc_` API key or an OAuth2 access token as the bearer credential, and scopes calls to a project via the tool's `containerTag` argument, the `x-mc-project` header, or the default project, in that order.

**Tools:** `memory` (save/forget), `recall` (search + profile), `listProjects`, `whoAmI`, `memory-graph`, `fetch-graph-data` · **Resources:** `memoricai://profile`, `memoricai://projects`, and a memory-graph UI stub · **Prompt:** `context`

Client configuration (Claude Code, or any client with native Streamable-HTTP support):

```json
{
  "mcpServers": {
    "memoricai": {
      "type": "http",
      "url": "http://localhost:7373/mcp",
      "headers": { "Authorization": "Bearer mc_YOUR_API_KEY" }
    }
  }
}
```

For stdio-only clients (e.g. Claude Desktop), bridge with [`mcp-remote`](https://www.npmjs.com/package/mcp-remote):

```json
{
  "mcpServers": {
    "memoricai": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "http://localhost:7373/mcp",
               "--header", "Authorization: Bearer mc_YOUR_API_KEY"]
    }
  }
}
```

Note: the transport is POST-only JSON-RPC (no server-initiated SSE stream); clients expecting the legacy HTTP+SSE transport will not work.

## Connectors

All connectors sync on a 4-hour cron (plus an immediate import on creation); every run is recorded in a sync ledger with status and error kind.
Only one sync may run per connection, provider item IDs are stable across runs, and the configured document limit applies to new items rather than updates. Connection API responses redact secret/token-like metadata fields.

| Provider | Auth | Live updates |
|---|---|---|
| GitHub | OAuth | Push/delete webhooks (HMAC-verified) when `MEMORICAI_GITHUB_WEBHOOK_SECRET` is set; cron fallback |
| Google Drive | OAuth | cron |
| Gmail | OAuth | cron |
| Notion | OAuth | cron |
| OneDrive | OAuth | cron |
| Granola | API key | cron |
| S3-compatible (AWS/MinIO/R2/Spaces) | Static keys (SigV4) | cron |
| Web crawler |, (start URL) | cron re-crawl; same-domain BFS with SSRF guard (DNS pre-resolution against private/metadata ranges) |

## How it works

```
ingest ──► extract ──► chunk ──► embed ──► LLM memory extraction
                                              │
                              relation inference + version chains
                              (similarity ≥ 0.97 → new version, ≥ 0.85 → "extends" edge)
                                              │
background: forgetting sweeper (60 s) · connector sync cron (4 h) · profile aggregation (6 h)
```

A Cargo workspace with downward-only dependencies:

```
memoricai (bin)          composition root, CLI, background workers, :7373
├── memoricai-api        axum routes (/v1), auth extractor, error shapes
├── memoricai-mcp        MCP Streamable-HTTP server (6 tools, 3 resources, 1 prompt)
├── memoricai-engine     ingest pipeline, memory extraction, temporal graph, search
├── memoricai-auth       API keys, scoped keys, OAuth2/OIDC provider, tenant policy
├── memoricai-models     pluggable LLM / embedding / rerank / transcribe / vision providers
├── memoricai-connectors Google Drive, Gmail, Notion, OneDrive, GitHub, Granola, S3, web crawler
├── memoricai-db         sqlx repositories + migrations (Postgres + pgvector)
└── memoricai-core       pure domain types, DTOs, MetadataFilter AST, trait ports
```

Vector search is an **exact scan** over pgvector cosine distance, correct at any scale, fast at small scale. Add HNSW/IVFFlat indexes per deployment once your embedding dimension is fixed.

## Development

```bash
cargo build                                   # debug build
cargo clippy --workspace --all-targets        # lints (workspace denies unsafe code)
cargo test --workspace                        # unit tests (no database needed)

# end-to-end test (real Postgres + pgvector):
createdb memoricai_test && psql -d memoricai_test -c 'CREATE EXTENSION IF NOT EXISTS vector;'
MEMORICAI_TEST_DATABASE_URL=postgres://$USER@localhost/memoricai_test \
  cargo test -p memoricai --test engine_e2e -- --ignored

# live HTTP smoke tests against a running server (~31 checks):
python3 scripts/smoke.py <keyfile>            # engine, API, MCP
python3 scripts/smoke_phase23.py <keyfile>    # OAuth, buckets, analytics, connectors, router
```

## Contributing

Issues and pull requests are welcome. Before submitting:

1. `cargo clippy --workspace --all-targets`, must be warning-free (unsafe code is rejected at compile time).
2. `cargo test --workspace`, include the Postgres e2e run if your change touches the engine, DB, or API.
3. Keep the dependency direction downward (`api/mcp → engine → db → core`) and put new domain types in `memoricai-core`.

## License

[MIT](LICENSE)
