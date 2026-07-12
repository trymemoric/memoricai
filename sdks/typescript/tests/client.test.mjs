import assert from "node:assert/strict";
import test from "node:test";

import { MemoricaiClient, MemoricaiError } from "../dist/index.js";

function jsonResponse(status, body) {
  return new Response(body === undefined ? "" : JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

test("sends auth, JSON, and a normalized base URL", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  let request;
  globalThis.fetch = async (url, init) => {
    request = { url, init };
    return jsonResponse(200, { id: "doc_1", status: "queued" });
  };

  const client = new MemoricaiClient("https://memory.example///", "mc_test");
  const result = await client.addText("Ada likes compilers", "project_a");

  assert.deepEqual(result, { id: "doc_1", status: "queued" });
  assert.equal(request.url, "https://memory.example/v1/documents");
  assert.equal(request.init.method, "POST");
  assert.equal(request.init.headers.Authorization, "Bearer mc_test");
  assert.equal(request.init.headers["Content-Type"], "application/json");
  assert.deepEqual(JSON.parse(request.init.body), {
    content: "Ada likes compilers",
    containerTag: "project_a",
  });
});

test("encodes untrusted document IDs in path segments", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  const urls = [];
  globalThis.fetch = async (url) => {
    urls.push(url);
    return jsonResponse(200, {});
  };
  const client = new MemoricaiClient("https://memory.example", "mc_test");

  await client.getDocument("a/b?x#y");
  await client.deleteDocument("../admin");

  assert.equal(urls[0], "https://memory.example/v1/documents/a%2Fb%3Fx%23y");
  assert.equal(urls[1], "https://memory.example/v1/documents/..%2Fadmin");
});

test("forgetMatching is dry-run by default and honors explicit false", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  const bodies = [];
  globalThis.fetch = async (_url, init) => {
    bodies.push(JSON.parse(init.body));
    return jsonResponse(200, { matches: [] });
  };
  const client = new MemoricaiClient("https://memory.example", "mc_test");

  await client.forgetMatching({ containerTag: "project_a", query: "old employer" });
  await client.forgetMatching({
    containerTag: "project_a",
    query: "old employer",
    dryRun: false,
  });

  assert.equal(bodies[0].dryRun, true);
  assert.equal(bodies[1].dryRun, false);
});

test("surfaces structured API errors without retrying permanent failures", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  let calls = 0;
  globalThis.fetch = async () => {
    calls += 1;
    return jsonResponse(400, { error: "bad_request", message: "invalid query" });
  };
  const client = new MemoricaiClient("https://memory.example", "mc_test");

  await assert.rejects(
    () => client.searchMemories({ q: "" }),
    (error) =>
      error instanceof MemoricaiError &&
      error.status === 400 &&
      error.message === "api error 400: invalid query",
  );
  assert.equal(calls, 1);
});

test("retries transient responses and returns the successful payload", async (t) => {
  const originalFetch = globalThis.fetch;
  const originalSetTimeout = globalThis.setTimeout;
  t.after(() => {
    globalThis.fetch = originalFetch;
    globalThis.setTimeout = originalSetTimeout;
  });
  const statuses = [503, 429, 200];
  globalThis.fetch = async () => {
    const status = statuses.shift();
    return status === 200
      ? jsonResponse(200, { status: "ok", version: "test" })
      : jsonResponse(status, { message: "retry" });
  };
  globalThis.setTimeout = (callback) => {
    callback();
    return 0;
  };
  const client = new MemoricaiClient("https://memory.example", "mc_test", {
    maxRetries: 2,
  });

  assert.deepEqual(await client.health(), { status: "ok", version: "test" });
  assert.deepEqual(statuses, []);
});
