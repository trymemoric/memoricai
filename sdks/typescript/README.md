# memoricai — TypeScript SDK

Zero-dependency client for the memoricai `/v1` HTTP API (Node 18+ / Bun / Deno — anything with global `fetch`).

```ts
import { MemoricaiClient } from "@memoricai/sdk";

const client = new MemoricaiClient("http://localhost:6767", "mc_...");

const doc = await client.addText("My name is Ada.", "mc_project_default");
await client.waitForDocument(doc.id);

const res = await client.searchMemories({
  q: "what is my name",
  containerTag: "mc_project_default",
  digest: true, // ready-to-inject, date-stamped context
});
console.log(res.digest);
```

Build with `npm run build` (or use `src/index.ts` directly under Bun). Transient
failures (429/5xx) are retried with exponential backoff; API errors throw
`MemoricaiError` with `status` and `message`.
