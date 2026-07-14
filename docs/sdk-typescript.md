# TypeScript SDK (`@memoricai/sdk`)

Zero-dependency client for the [`/v1` HTTP API](api.md), built on the global
`fetch`, works on Node 18+, Bun, Deno, and edge runtimes. Fully typed request
and response interfaces; ESM.

```bash
npm install @memoricai/sdk
```

## Quickstart

```ts
import { MemoricaiClient } from "@memoricai/sdk";

const client = new MemoricaiClient("http://localhost:7373", "mc_...");

const doc = await client.addText("My name is Ada.", "mc_project_default");
await client.waitForDocument(doc.id);

const res = await client.searchMemories({
  q: "what is my name",
  containerTag: "mc_project_default",
  digest: true, // ready-to-inject, date-stamped context
});
console.log(res.digest);
```

## Construction & behavior

```ts
new MemoricaiClient(baseUrl, apiKey, options?)
// options: { timeoutMs?: number /* 120000 */, maxRetries?: number /* 4 */ }
```

- Transient failures (**429, 500, 502, 503**) are retried with exponential
  backoff (1 s, 2 s, 4 s, …) up to `maxRetries` times.
- Other non-2xx responses throw **`MemoricaiError`** with `.status` and
  `.message` from the server's error envelope.
- Every request is bounded by `AbortSignal.timeout(timeoutMs)`.
- `waitForDocument` throws `MemoricaiError(500, …)` if the document reaches
  `failed` and `MemoricaiError(408, …)` past its deadline.

## Methods

### Documents

```ts
addDocument(req: AddDocumentRequest): Promise<IngestResponse>   // POST /v1/documents
addText(content: string, containerTag: string): Promise<IngestResponse>
getDocument(id: string): Promise<MemoricaiDocument>             // GET /v1/documents/{id}
deleteDocument(id: string): Promise<unknown>                    // DELETE /v1/documents/{id}
listDocuments(req: { containerTags?; page?; limit?; status? }): Promise<DocumentListResponse>
searchDocuments(req: DocumentSearchRequest): Promise<DocumentSearchResponse>
waitForDocument(id: string, timeoutMs = 120_000): Promise<MemoricaiDocument>
```

`AddDocumentRequest`: `content` (required), `containerTag`, `containerTags`,
`customId`, `metadata`, `entityContext`, `contentType`, `title`.

`DocumentSearchRequest`: `q` (required), `containerTags`, `limit`,
`chunkThreshold`, `documentThreshold`, `docId`, `includeFullDocs`,
`includeSummary`, `rerank`, `rewriteQuery`, `filters`.

### Search & profile

```ts
searchMemories(req: MemorySearchRequest): Promise<MemorySearchResponse> // POST /v1/search
buildContext(req: ContextRequest): Promise<ContextResponse>             // POST /v1/context
profile(req: { containerTag; q?; threshold?; include?; buckets? }): Promise<ProfileResponse>
```

`MemorySearchRequest`: `q` (required), `containerTag`,
`searchMode?: "memories" | "hybrid" | "documents"`, `limit`, `threshold`,
`rerank`, `rewriteQuery`, `filters`, `include`, and **`digest?: boolean`**, when true the response carries `digest`, the compact date-stamped context block
(see the [API docs](api.md#post-v1search)).

`buildContext` accepts `q`, `containerTag`, `mode`, `budgetTokens`,
`maxSources`, `threshold`, `rewriteQuery`, `filters`, and `includeDigest`; the
response includes the bounded context, structured evidence, and packing diagnostics.

### Memories

```ts
createMemories(containerTag: string, memories: MemoryInput[]):
    Promise<{ memories: MemoricaiMemory[] }>               // POST /v1/memories
patchMemory(req: { id; newContent; metadata? }): Promise<MemoricaiMemory>  // PATCH
forgetMemory(req: { containerTag; id?; content?; reason? }): Promise<MemoricaiMemory> // DELETE
forgetMatching(req: { containerTag; query; threshold?; maxForget?;
                      dryRun?; reason? }): Promise<unknown> // POST /v1/memories/forget-matching
```

`MemoryInput`: `{ content: string; isStatic?: boolean; metadata? }`.
Pass `dryRun: true` to `forgetMatching` to preview candidates without
forgetting (the server default is a real forget).

### Misc

```ts
health(): Promise<{ status: string; version: string }>      // GET /health
```

## Exported types

`MemoricaiClient` (default export too), `MemoricaiError`, and interfaces:
`AddDocumentRequest`, `IngestResponse`, `MemoricaiDocument`,
`DocumentListResponse`, `DocumentSearchRequest`, `DocumentSearchResult`,
`DocumentSearchResponse`, `ChunkHit`, `MemorySearchRequest`,
`MemorySearchResult`, `MemorySearchResponse`, `Profile`, `ProfileResponse`,
`ContextRequest`, `ContextEvidence`, `ContextDiagnostics`, `ContextResponse`,
`MemoryInput`, `MemoricaiMemory`, `ClientOptions`.
