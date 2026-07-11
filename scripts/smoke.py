#!/usr/bin/env python3
import json, time, urllib.request, urllib.error, sys, pathlib

BASE = "http://127.0.0.1:6767"
KEY = pathlib.Path(sys.argv[1]).read_text().strip()
TAG = "mc_project_smoke"
results = []

def call(method, path, body=None, auth=True, expect=None):
    url = BASE + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if auth:
        req.add_header("Authorization", "Bearer " + KEY)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            code = r.status
            payload = r.read().decode()
    except urllib.error.HTTPError as e:
        code = e.code
        payload = e.read().decode()
    try:
        parsed = json.loads(payload) if payload else {}
    except Exception:
        parsed = {"_raw": payload}
    return code, parsed

def check(name, cond, detail=""):
    results.append((name, cond, detail))
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f"  -- {detail}" if detail and not cond else ""))

# 1. wait for health
for _ in range(60):
    try:
        c, _ = call("GET", "/health", auth=False)
        if c == 200:
            break
    except Exception:
        pass
    time.sleep(0.5)
else:
    print("server never became healthy"); sys.exit(2)
print("server healthy\n")

# 2. auth required
c, _ = call("GET", "/v1/session", auth=False)
check("unauthenticated request -> 401", c == 401, f"got {c}")

# 3. session with key
c, sess = call("GET", "/v1/session")
check("GET /v1/session returns user+org", c == 200 and "user" in sess and "org" in sess, f"{c} {sess}")

# 4. ingest
c, ing = call("POST", "/v1/documents", {
    "content": "My name is Ada Lovelace and I love Rust programming and the analytical engine.",
    "containerTag": TAG,
    "metadata": {"topic": "intro"},
})
doc_id = ing.get("id")
check("POST /v1/documents -> queued", c == 200 and ing.get("status") == "queued" and bool(doc_id), f"{c} {ing}")

# 5. poll until done
status = None
for _ in range(60):
    c, doc = call("GET", f"/v1/documents/{doc_id}")
    status = doc.get("status")
    if status in ("done", "failed"):
        break
    time.sleep(0.3)
check("document reaches status=done", status == "done", f"status={status}")

# 6. list documents
c, lst = call("POST", "/v1/documents/list", {"containerTags": [TAG], "limit": 10})
check("POST /v1/documents/list contains the doc",
      c == 200 and any(d.get("id") == doc_id for d in lst.get("memories", [])),
      f"{c} count={len(lst.get('memories', []))}")

# 7. memory search
c, s4 = call("POST", "/v1/search", {"q": "What is my name?", "containerTag": TAG, "searchMode": "hybrid", "threshold": 0.05})
mems = [r.get("memory") or r.get("chunk") or "" for r in s4.get("results", [])]
found = any("Ada" in (m or "") for m in mems)
check("POST /v1/search finds the memory", c == 200 and found, f"{c} results={mems[:3]}")

# 8. document/chunk search
c, s3 = call("POST", "/v1/documents/search", {"q": "analytical engine rust", "containerTags": [TAG], "chunkThreshold": 0.05, "documentThreshold": 0.05})
check("POST /v1/documents/search returns a document", c == 200 and s3.get("total", 0) >= 1, f"{c} total={s3.get('total')}")

# 9. profile reflects the ingested memory (a real extractor may classify
# identity facts as static, so accept either bucket)
c, prof = call("POST", "/v1/profile", {"containerTag": TAG})
dynamic = prof.get("profile", {}).get("dynamic") or []
static = prof.get("profile", {}).get("static") or []
check("POST /v1/profile has memories", c == 200 and len(dynamic) + len(static) >= 1,
      f"{c} dynamic={dynamic[:2]} static={static[:2]}")

# 10. direct memory create (static) -> shows in profile static
c, cm = call("POST", "/v1/memories", {"containerTag": TAG, "memories": [{"content": "Prefers dark mode.", "isStatic": True}]})
check("POST /v1/memories creates memory", c == 200 and len(cm.get("memories", [])) == 1, f"{c} {cm}")
c, prof2 = call("POST", "/v1/profile", {"containerTag": TAG})
statics = prof2.get("profile", {}).get("static") or []
check("static memory appears in profile", any("dark mode" in s for s in statics), f"static={statics[:3]}")

# 11. scoped key mint
c, sk = call("POST", "/v1/auth/scoped-key", {"containerTag": TAG, "name": "smoke-scoped"})
check("POST /v1/auth/scoped-key mints mc_ key", c == 200 and sk.get("key", "").startswith("mc_"), f"{c}")
scoped = sk.get("key", "")
# scoped key blocked on /v1/settings (not allowlisted)
if scoped:
    req = urllib.request.Request(BASE + "/v1/settings", method="GET")
    req.add_header("Authorization", "Bearer " + scoped)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            sc = r.status
    except urllib.error.HTTPError as e:
        sc = e.code
    check("scoped key forbidden on /v1/settings", sc == 403, f"got {sc}")

# 12. MCP initialize (no auth) + tools/call recall (auth)
c, mcp_init = call("POST", "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}, auth=False)
si = (mcp_init.get("result", {}) or {}).get("serverInfo", {})
check("MCP initialize -> serverInfo", c == 200 and si.get("name") == "memoricai-mcp", f"{c} {mcp_init.get('result', {})}")

c, mcp_recall = call("POST", "/mcp", {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
    "params": {"name": "recall", "arguments": {"query": "my name", "containerTag": TAG}}})
content = json.dumps(mcp_recall.get("result", {}))
check("MCP recall returns content", c == 200 and ("Ada" in content or "Memories" in content), f"{c} {content[:200]}")

# summary
print()
passed = sum(1 for _, ok, _ in results if ok)
total = len(results)
print(f"==== E2E: {passed}/{total} checks passed ====")
sys.exit(0 if passed == total else 1)
