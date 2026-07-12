# memoricai HTTP API (`/v1`)

Base URL: wherever `memoricai serve` listens (default `http://localhost:7373`). All request/response bodies are JSON in camelCase. Request bodies are capped at 12 MiB.

SDKs wrap this API 1:1, see [sdk-rust.md](sdk-rust.md), [sdk-python.md](sdk-python.md), [sdk-typescript.md](sdk-typescript.md).

## Authentication

Every endpoint (except `/health`, OAuth discovery, and MCP initialize) requires:

```
Authorization: Bearer mc_...
```

Three credential kinds are accepted:

| Credential | Scope |
|---|---|
| Organization API key (`mc_...`, printed at first boot or minted via `memoricai key create`) | Full access to the organization |
| Container-scoped key (`POST /v1/auth/scoped-key`) | One container tag, fixed endpoint allowlist, own rate limit |
| OAuth2 access token (built-in OIDC provider; used by MCP clients) | Intersection of the token's scopes and the user's membership |

Scoped keys may only call: `/v1/documents*`, `/v1/memories*`, `/v1/search`, `/v1/profile`, `/v1/router`, `/v1/session`.

## Errors

Non-2xx responses carry a stable envelope:

```json
{ "error": "bad_request", "message": "limit must be between 1 and 100" }
```

| Status | `error` code | Meaning |
|---|---|---|
| 400 | `bad_request` | Validation failure (message says which field) |
| 401 | `unauthorized` | Missing/invalid credential |
| 402 | `payment_required` | Quota exhausted |
| 403 | `forbidden` | Credential lacks permission (role, container scope, endpoint allowlist) |
| 404 | `not_found` | Resource does not exist in this tenant |
| 409 | `conflict` | Concurrent-update conflict |
| 429 | `rate_limited` | Scoped-key rate limit exceeded |
| 500 | `database` / `internal` | Server fault |
| 502 | `model` | Upstream model provider failed |

## Container tags

All content is partitioned by **container tag** (`^[a-zA-Z0-9_:\-.]+$`, 1-100 chars), one tag per project/end-user, e.g. `mc_project_default`, `user_42`. Search never crosses tags unless you pass several.

## Metadata filters

Endpoints with a `filters` field accept a recursive AND/OR tree over document/memory metadata (as a JSON object, or a JSON-encoded string of one):

```json
{ "AND": [
    { "key": "topic",  "value": "rust",  "filterType": "string_equal", "ignoreCase": true },
    { "OR": [
        { "key": "priority", "value": 3, "filterType": "numeric", "numericOperator": ">=" },
        { "key": "tags", "value": "urgent", "filterType": "array_contains" }
    ]}
]}
```

Leaf fields: `key`, `value`, `filterType` (`string_equal` | `string_contains` | `numeric` | `array_contains`; default `string_equal`), `numericOperator` (`<` `<=` `>` `>=` `=` `!=`), `negate` (bool), `ignoreCase` (bool).

---

## Documents

Content is accepted instantly and processed asynchronously: `queued → extracting → chunking → embedding → indexing → done | failed`. Poll `GET /v1/documents/{id}` for status.

### `POST /v1/documents`, ingest

```json
{
  "content": "My name is Ada and I love Rust.",
  "containerTag": "mc_project_default",
  "customId": "note-1",
  "metadata": { "topic": "intro" },
  "entityContext": "Ada is the end user",
  "contentType": "text",
  "title": "Intro note",
  "raw": null
}
```

`content` (required, ≤ 10 MiB) may be plain text, Markdown, code, or a URL (fetched with SSRF guards). `containerTag` or `containerTags` scopes it. `metadata` ≤ 256 KiB. Re-submitting the same `customId` reprocesses the document.

Response `200`: `{ "id": "doc_…", "status": "queued" }`

### `POST /v1/documents/batch`, batch ingest

`{ "documents": [ <ingest body>… ], "containerTag": …, "entityContext": …, "metadata": … }` (per-item fields win). Response: `{ "results": [{ "id", "status", "error?" }…], "success": n, "failed": n }`

### `POST /v1/documents/file`, file upload

`multipart/form-data` with the file part (+ optional `containerTag` field). PDFs, images (with a vision model configured), and audio/video (with a transcriber) are extracted server-side.

### `GET /v1/documents` / `POST /v1/documents/list`, list

POST body: `{ "page": 1, "limit": 20, "containerTags": ["…"], "sort": "createdAt", "order": "desc", "status": "done" }`.
Response: `{ "memories": [Document…], "pagination": { "currentPage", "limit", "totalItems", "totalPages" } }`

### `GET /v1/documents/{id}` · `PATCH /v1/documents/{id}` · `DELETE /v1/documents/{id}`

Get returns the full document including `content`, `status`, `metadata`, `containerTags`, timestamps. PATCH accepts ingest-shaped fields and reprocesses. DELETE removes the document, its chunks, and its derived memories (version chains repair themselves: a superseded predecessor becomes latest again). `{id}` may be the internal id or a `customId`.

### `GET /v1/documents/processing`, documents not yet `done` · `DELETE /v1/documents/bulk`, bulk delete

### `POST /v1/documents/search`, chunk-level RAG

```json
{
  "q": "what vector indexes does postgres support",
  "containerTags": ["mc_project_default"],
  "limit": 10,
  "chunkThreshold": 0.5,
  "documentThreshold": 0.5,
  "docId": null,
  "includeFullDocs": false,
  "includeSummary": false,
  "rerank": false,
  "rewriteQuery": false,
  "filters": null
}
```

- `q`: 1-4096 bytes. `limit`: 1-100.
- Thresholds are cosine-similarity floors in `[0,1]` (chunk-level and best-chunk-per-document).
- `docId` restricts the search to one document.
- `includeFullDocs` adds each result's full `content`; `includeSummary` adds its stored summary.
- `rewriteQuery` expands the query into variations with the configured LLM (adds a model round-trip); `rerank` re-scores results (remote rerank endpoint if configured, else LLM).

Response: `{ "results": [{ "documentId", "title?", "type", "score", "chunks": [{ "content", "score", "isRelevant" }], "metadata", "content?", "summary?", "createdAt", "updatedAt" }], "timing": <ms>, "total": n }`

A `GET /v1/documents/search?q=…&limit=…` variant exists with defaults for everything else.

---

## Memory search

### `POST /v1/search`

```json
{
  "q": "what is my name",
  "containerTag": "mc_project_default",
  "searchMode": "hybrid",
  "limit": 10,
  "threshold": 0.5,
  "digest": true,
  "rerank": false,
  "rewriteQuery": false,
  "filters": null,
  "include": { "documents": false, "relatedMemories": false, "forgottenMemories": false }
}
```

- `searchMode`: `memories` (extracted facts only), `hybrid` (facts, backfilled with document chunks when results are scarce, the default), `documents` (chunks only).
- Only the **latest version** of each memory is searchable; superseded and forgotten memories are excluded (set `include.forgottenMemories` to search forgotten ones).
- `include.relatedMemories` attaches version-graph context (parents/children); `include.documents` attaches the source document.
- **`digest: true`** additionally returns a compact, ready-to-inject context block: the top matching facts grouped by source session/document, date-stamped (using the document's `date` metadata when present and each fact's extracted `eventDate`), latest versions only, ~4k-char budget. Aggregation-shaped queries (“how many…”, “list all…”) automatically widen the digest (up to 200 memories, 8k chars) because completeness beats top-k relevance for counting. Composition is deterministic, no model calls.

Response: `{ "results": [{ "id", "memory?", "chunk?", "similarity", "metadata", "updatedAt", "version", "rootMemoryId?", "context?", "documents?" }], "timing": <ms>, "total": n, "digest?": "…" }`

---

## Profile

### `POST /v1/profile`

`{ "containerTag": "…", "q?": "…", "threshold?": 0.5, "filters?": …, "include?": ["static","dynamic","buckets"], "buckets?": ["health"] }`

Returns the container's auto-maintained profile, `static` (identity facts and lasting preferences), `dynamic` (recent facts + periodic `[Summary]` aggregations), `buckets` (topical groupings), from a fast path with no model calls. Passing `q` additionally runs a hybrid memory search and returns it as `searchResults`.

---

## Memories (direct management)

### `POST /v1/memories`, create without extraction

`{ "containerTag": "…", "memories": [{ "content": "Prefers dark mode.", "isStatic": true, "metadata?": {…} }] }` → `{ "memories": [{ id, … }] }`

### `PATCH /v1/memories`, versioned update

`{ "id": "mem_…", "newContent": "…", "metadata?": {…} }`, appends a new version (old one retires but remains in history), returns the new `Memory`.

### `DELETE /v1/memories`, forget one

`{ "containerTag": "…", "id?": "mem_…", "content?": "exact content", "reason?": "…" }`, exactly one of `id`/`content`. Soft-delete: excluded from search, retained for history.

### `POST /v1/memories/forget-matching`, semantic bulk forget

`{ "containerTag": "…", "query": "…", "threshold": 0.5, "maxForget": 100, "dryRun": false, "reason?": "…" }`
With `dryRun: true` returns the candidates without forgetting. Response: `{ "dryRun", "count", "forgetBatchId?", "summary", "candidates?", "forgotten?" }`

---

## Projects & container tags

- `GET /v1/projects` / `POST /v1/projects` (`{ "name", "emoji?" }`) / `DELETE /v1/projects/{id}`
- `GET|POST /v1/container-tags/list`, `PATCH|DELETE /v1/container-tags/{tag}`
- `GET /v1/container-tags/{tag}/inferred` + `POST /v1/container-tags/{tag}/inferred/{memoryId}/review`, list and approve/reject inferred memories

## Settings

`GET /v1/settings` · `PATCH /v1/settings` · `POST /v1/settings/reset`

PATCH fields (all optional): `shouldLlmFilter`, `filterPrompt`, `categories`, `includeItems`, `excludeItems` (extraction filtering policy), `chunkSize` (target chunk characters).

## Auth & session

- `GET /v1/session` → `{ "user": {…}, "org": {…} }` for the presented credential
- `POST /v1/auth/scoped-key` → mint a container-scoped key: `{ "containerTag", "name?", "expiresInDays?", "rateLimitMax?", "rateLimitTimeWindow?" }` → `{ "key", "id", "name", "containerTag", "expiresAt?", "allowedEndpoints" }`. The key is shown once.
- `DELETE /v1/auth/scoped-key/{id}` → revoke

## Analytics

`GET /v1/analytics/{usage,errors,logs,memory,chat}?period=&page=&limit=`, `period`: `24h`, `7d`, `30d` (default), `90d`, `all`.

## Memory Router (LLM proxy)

`POST /v1/router/{*target}`, OpenAI-compatible proxy that injects relevant memories into chat requests before forwarding. Headers: `Authorization` carries the **upstream** provider key, `x-memoricai-api-key` the memoricai key, optional `x-mc-project` selects the container. Upstream origins must be HTTPS or allowlisted via `MEMORICAI_ROUTER_ALLOWED_ORIGINS`.

## Misc

- `GET /health` → `{ "service", "status", "version" }` (no auth)
- `GET /v1/openapi` → machine-readable endpoint summary
- `POST /mcp`, MCP Streamable-HTTP server (see README → MCP server)
- OAuth2/OIDC: `/api/auth/oauth2/{authorize,consent,token,register}` + `/.well-known/{oauth-authorization-server,openid-configuration}`
- Connections (data connectors): `GET|POST /v1/connections`, `POST /v1/connections/{id}/import`, `GET /v1/connections/{id}/{sync-runs,resources}`, provider OAuth callbacks and webhooks

## Admin (provisioning)

`POST /v1/admin/provision` creates an isolated organization (+ owner user + org API key) in one call — meant for a control plane to invoke once per customer signup, not for end users.

Disabled by default: unless `MEMORICAI_PROVISION_KEY` is set, the route returns a plain **404** (it hides its existence rather than advertising that it needs auth).

Auth: `Authorization: Bearer <MEMORICAI_PROVISION_KEY>` (compared in constant time; not a `mc_...` API key, and the regular `Auth` extractor is not used for this route).

```json
// request
{ "orgName": "Acme Inc", "email": "owner@acme.com" }
```

```json
// 201 response
{
  "orgId": "org_…",
  "orgName": "Acme Inc",
  "userId": "user_…",
  "apiKey": "mc_…"
}
```

`apiKey` is the plaintext full-access org key, shown once — store it immediately.

**Production warning:** this endpoint mints unrestricted org credentials for anyone holding the master key. Network-restrict `/v1/admin` (reverse proxy / firewall rule) to trusted control-plane callers only; do not expose it publicly.
