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
  const headers = new Headers(request.init.headers);
  assert.equal(headers.get("Authorization"), "Bearer mc_test");
  assert.equal(headers.get("Content-Type"), "application/json");
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

test("buildContext uses the bounded context endpoint", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  let request;
  globalThis.fetch = async (url, init) => {
    request = { url, body: JSON.parse(init.body) };
    return jsonResponse(200, {
      context: "Relevant source excerpts:\n",
      evidence: [],
      diagnostics: {},
      timing: 1,
    });
  };
  const client = new MemoricaiClient("https://memory.example", "mc_test");

  await client.buildContext({ q: "count visits", budgetTokens: 1000, maxSources: 5 });

  assert.equal(request.url, "https://memory.example/v1/context");
  assert.deepEqual(request.body, {
    q: "count visits",
    budgetTokens: 1000,
    maxSources: 5,
  });
});

test("covers management and connector routes with encoded path segments", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  const requests = [];
  globalThis.fetch = async (url, init) => {
    requests.push({ url, init });
    return jsonResponse(200, {});
  };
  const client = new MemoricaiClient("https://memory.example", "mc_test");

  await client.updateContainerTag("team/a", { name: "Team A" });
  await client.connectionResources("conn/a", { page: 2, perPage: 50 });
  await client.createScopedKey({ containerTag: "team/a", rateLimitMax: 100 });

  assert.equal(requests[0].url, "https://memory.example/v1/container-tags/team%2Fa");
  assert.equal(
    requests[1].url,
    "https://memory.example/v1/connections/conn%2Fa/resources?page=2&perPage=50",
  );
  assert.equal(requests[2].url, "https://memory.example/v1/auth/scoped-key");
});

test("router keeps upstream auth separate and returns a streamable response", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  let request;
  globalThis.fetch = async (url, init) => {
    request = { url, init };
    return new Response("data: done\n\n", {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  };
  const client = new MemoricaiClient("https://memory.example", "mc_test");

  const response = await client.routerRequest(
    "https://api.example/v1/chat/completions?api-version=1",
    { stream: true, messages: [] },
    "upstream-key",
    "project-a",
  );

  assert.equal(
    request.url,
    "https://memory.example/v1/router/https://api.example/v1/chat/completions%3Fapi-version=1",
  );
  const headers = new Headers(request.init.headers);
  assert.equal(headers.get("Authorization"), "Bearer upstream-key");
  assert.equal(headers.get("x-memoricai-api-key"), "mc_test");
  assert.equal(headers.get("x-mc-project"), "project-a");
  assert.equal(await response.text(), "data: done\n\n");
});

test("OAuth token exchange uses form encoding", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  let request;
  globalThis.fetch = async (url, init) => {
    request = { url, init };
    return jsonResponse(200, { access_token: "token", token_type: "Bearer", expires_in: 3600 });
  };
  const client = new MemoricaiClient("https://memory.example", "");

  await client.exchangeOAuthToken({
    grant_type: "authorization_code",
    client_id: "client_1",
    code: "code with spaces",
    redirect_uri: "http://localhost/callback",
  });

  assert.equal(request.url, "https://memory.example/api/auth/oauth2/token");
  assert.equal(new Headers(request.init.headers).get("Content-Type"), "application/x-www-form-urlencoded");
  assert.equal(request.init.body.get("code"), "code with spaces");
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
