# Configuration

All memoricai configuration is provided through environment variables. The server requires a Postgres database, a chat endpoint, and an embeddings endpoint unless in-process embeddings are enabled.

## Model setup examples

memoricai works with OpenAI-compatible providers, whether hosted or fully local.

### OpenAI

`OPENAI_BASE_URL` also enables audio transcription.

```bash
export MEMORICAI_LLM_BASE_URL=https://api.openai.com/v1
export MEMORICAI_LLM_MODEL=gpt-4o-mini
export MEMORICAI_EMBEDDING_BASE_URL=https://api.openai.com/v1
export MEMORICAI_EMBEDDING_MODEL=text-embedding-3-small
export MEMORICAI_EMBEDDING_MODEL_VERSION=provider-default
export MEMORICAI_EMBEDDING_DIM=1536
export OPENAI_API_KEY=sk-...
```

### In-process embeddings

Build with `--features local-embeddings`; no embeddings API is required.

```bash
export MEMORICAI_EMBEDDING_PROVIDER=local
export MEMORICAI_EMBEDDING_MODEL=nomic-embed-text-v1.5-q
```

Supported local models are `nomic-embed-text-v1.5-q`, `nomic-embed-text-v1.5`, `bge-small-en-v1.5`, and `all-minilm-l6-v2`.

### Ollama

```bash
export MEMORICAI_LLM_BASE_URL=http://localhost:11434/v1
export MEMORICAI_LLM_MODEL=llama3.1
export MEMORICAI_EMBEDDING_BASE_URL=http://localhost:11434/v1
export MEMORICAI_EMBEDDING_MODEL=nomic-embed-text
export MEMORICAI_EMBEDDING_DIM=768
```

`OPENAI_BASE_URL`, `OPENAI_API_KEY`, and `OPENAI_MODEL` are accepted as fallbacks for LLM, embedding, and transcription settings. A plain OpenAI setup therefore needs only `OPENAI_BASE_URL` and `OPENAI_API_KEY`.

## Core

| Variable | Default | Purpose |
|---|---|---|
| `MEMORICAI_DATABASE_URL` | **required** | Postgres connection string (`DATABASE_URL` also accepted) |
| `MEMORICAI_BIND` | `0.0.0.0:7373` | HTTP listen address |
| `MEMORICAI_INGEST_CONCURRENCY` | CPU count, clamped 2–8 | Ingest worker pool size; raise it for bulk imports |
| `MEMORICAI_BASE_URL` | Loopback request origin only | Canonical HTTPS public origin for OAuth discovery, connector callbacks, and webhooks; required for non-loopback deployments |
| `MEMORICAI_ROUTER_ALLOWED_ORIGINS` | Public HTTPS origins | Optional comma-separated exact upstream origins for the Memory Router; required for HTTP or private-network model servers |
| `MEMORICAI_CONNECTOR_ALLOWED_ORIGINS` | None | Optional comma-separated exact origins allowed to reach private-network S3-compatible endpoints |
| `MEMORICAI_ENV` | Release: `production`; debug: `development` | Runtime mode; release binaries fail closed unless explicitly set to `development`, `dev`, `local`, or `test` |
| `MEMORICAI_ENCRYPTION_KEY` | None | 32-byte base64 or hex AES-256-GCM key for connector tokens, provider cursors, and sensitive metadata; required in production |
| `MEMORICAI_REQUIRE_ENCRYPTION` | `false` | Require an encryption key without changing the environment name |
| `MEMORICAI_MAX_INFLIGHT_REQUESTS` | `256` | Global cap on concurrently executing HTTP requests (1–10000) |
| `MEMORICAI_REQUEST_BODY_TIMEOUT_SECONDS` | `30` | Maximum request-body read time (1–300 seconds) |
| `MEMORICAI_ANALYTICS_RETENTION_DAYS` | `90` | Delete request analytics older than this many days (1–3650) |
| `MEMORICAI_PROVISION_KEY` | None; endpoint disabled | Master credential for `POST /v1/admin/provision`; when unset, the endpoint returns 404 |
| `RUST_LOG` | `info,memoricai=debug` | Log filter using tracing `EnvFilter` syntax |

Generate an encryption key with `openssl rand -base64 32`.

## Models

| Variable | Default | Purpose |
|---|---|---|
| `MEMORICAI_LLM_BASE_URL` | **required** | OpenAI-compatible chat endpoint; fallback `OPENAI_BASE_URL` |
| `MEMORICAI_LLM_MODEL` | `gpt-4o-mini` | Chat model; fallbacks `OPENAI_MODEL`, `MEMORICAI_MODEL` |
| `MEMORICAI_LLM_API_KEY` | None | Chat authentication; fallback `OPENAI_API_KEY` |
| `MEMORICAI_EMBEDDING_BASE_URL` | **required** | Embeddings endpoint; fallback `OPENAI_BASE_URL` |
| `MEMORICAI_EMBEDDING_MODEL` | `text-embedding-3-small` | Embedding model |
| `MEMORICAI_EMBEDDING_MODEL_VERSION` | `provider-default` | Explicit model revision or weight version; changing it creates an index and queues re-embedding |
| `MEMORICAI_EMBEDDING_PROVIDER_NAME` | Endpoint hostname | Stable provider identity recorded with remote vector indexes |
| `MEMORICAI_EMBEDDING_API_KEY` | None | Embeddings authentication; fallback `OPENAI_API_KEY` |
| `MEMORICAI_EMBEDDING_DIM` | `1536` | Vector dimension; changing it creates an index and queues re-embedding |
| `MEMORICAI_RERANK_URL` | LLM-based reranking | Dedicated rerank endpoint using a TEI, Jina, or Cohere-style API |
| `MEMORICAI_RERANK_MODEL` / `_API_KEY` | `rerank` / none | Rerank model and authentication |
| `MEMORICAI_TRANSCRIBE_BASE_URL` | Disabled | Audio and video transcription endpoint; fallback `OPENAI_BASE_URL` |
| `MEMORICAI_TRANSCRIBE_MODEL` / `_API_KEY` | `whisper-1` / none | Transcription model and authentication |
| `MEMORICAI_VISION_BASE_URL` | Disabled | Image captioning and OCR endpoint; no `OPENAI_BASE_URL` fallback |
| `MEMORICAI_VISION_MODEL` / `_API_KEY` | `gpt-4o-mini` / none | Vision model and authentication |

Embedding vectors are stored in versioned per-organization indexes identified by provider, model ID, model version, and dimension. Memory and chunk rows retain their source text, so changing any identity field creates a distinct index and durably re-embeds missing vectors in background batches. Old indexes remain isolated and are never compared with queries produced by the newly configured model.

Set `MEMORICAI_EMBEDDING_MODEL_VERSION` to a pinned provider or model revision when one is available. The `provider-default` value cannot detect a provider silently replacing weights behind an unchanged model ID.

## Connectors

| Variable | Default | Purpose |
|---|---|---|
| `MEMORICAI_<PROVIDER>_CLIENT_ID` / `_CLIENT_SECRET` | None | OAuth application credentials per provider, such as `MEMORICAI_GOOGLE_DRIVE_CLIENT_ID` |
| `MEMORICAI_GITHUB_WEBHOOK_SECRET` | None | Enables HMAC-SHA256-verified GitHub push/delete webhooks; when unset, GitHub uses cron polling |
| `MEMORICAI_GRANOLA_BASE_URL` | `https://api.granola.ai` | Granola API base URL |
