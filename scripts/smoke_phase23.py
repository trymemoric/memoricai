#!/usr/bin/env python3
"""Phase 2/3 live smoke test: OAuth2 flow, buckets, analytics, inferred review,
connectors, Memory Router, and MCP-over-OAuth. Usage: smoke_phase23.py <keyfile>"""
import base64, hashlib, json, sys, pathlib, time, urllib.request, urllib.error, urllib.parse

BASE = "http://127.0.0.1:6767"
KEY = pathlib.Path(sys.argv[1]).read_text().strip()
TAG = "mc_project_smoke"
results = []


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a):
        return None


def call(method, path, body=None, auth=None, headers=None, form=False, follow=True):
    url = BASE + path
    data = None
    hdrs = dict(headers or {})
    if body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode()
            hdrs["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body).encode()
            hdrs["Content-Type"] = "application/json"
    if auth:
        hdrs["Authorization"] = "Bearer " + auth
    req = urllib.request.Request(url, data=data, method=method, headers=hdrs)
    opener = urllib.request.build_opener() if follow else urllib.request.build_opener(NoRedirect)

    def lower_headers(h):
        return {k.lower(): v for k, v in h.items()}

    try:
        with opener.open(req, timeout=30) as r:
            return r.status, r.read().decode(), lower_headers(r.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(), lower_headers(e.headers)


def j(payload):
    try:
        return json.loads(payload)
    except Exception:
        return {}


def check(name, cond, detail=""):
    results.append(cond)
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f"  -- {detail}" if detail and not cond else ""))


# wait for health
for _ in range(60):
    try:
        if call("GET", "/health")[0] == 200:
            break
    except Exception:
        pass
    time.sleep(0.5)
print("server healthy\n")

# --- OAuth2 provider full flow ---
c, body, _ = call("POST", "/api/auth/oauth2/register",
                  {"redirect_uris": ["http://localhost:9999/callback"], "client_name": "smoke",
                   "token_endpoint_auth_method": "none"})
client_id = j(body).get("client_id", "")
check("DCR register -> client_id", c == 200 and client_id.startswith("client_"), f"{c} {body[:120]}")

verifier = base64.urlsafe_b64encode(b"a" * 40).decode().rstrip("=")
challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip("=")
c, body, hdrs = call("POST", "/api/auth/oauth2/consent",
                     {"api_key": KEY, "client_id": client_id, "redirect_uri": "http://localhost:9999/callback",
                      "code_challenge": challenge, "code_challenge_method": "S256", "state": "xyz",
                      "permission": "write", "container_tags": TAG},
                     form=True, follow=False)
loc = hdrs.get("location", "")
code = urllib.parse.parse_qs(urllib.parse.urlparse(loc).query).get("code", [""])[0]
check("consent -> 302 redirect with code", c in (302, 303) and bool(code), f"{c} loc={loc[:80]}")

c, body, _ = call("POST", "/api/auth/oauth2/token",
                  {"grant_type": "authorization_code", "client_id": client_id, "code": code,
                   "redirect_uri": "http://localhost:9999/callback", "code_verifier": verifier},
                  form=True)
access = j(body).get("access_token", "")
refresh = j(body).get("refresh_token", "")
check("token exchange (PKCE) -> access_token", c == 200 and bool(access), f"{c} {body[:120]}")

# Use the access token BEFORE testing refresh (refresh revokes the old access token).
c, body, _ = call("GET", "/v1/mcp/session-with-key", auth=access)
minted = j(body).get("apiKey", "")
check("session-with-key mints mc_ api key", c == 200 and minted.startswith("mc_"), f"{c} {body[:120]}")

c, body, _ = call("GET", "/v1/session", auth=minted)
check("minted key authenticates", c == 200 and "user" in j(body), f"{c}")

# MCP over OAuth token (still using the un-revoked access token).
c, body, _ = call("POST", "/mcp",
                  {"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                   "params": {"name": "whoAmI", "arguments": {}}},
                  auth=access)
check("MCP accepts OAuth access token", c == 200 and "result" in j(body), f"{c} {body[:120]}")

c, body, _ = call("POST", "/api/auth/oauth2/token",
                  {"grant_type": "refresh_token", "client_id": client_id,
                   "refresh_token": refresh}, form=True)
check("refresh_token grant works", c == 200 and j(body).get("access_token"), f"{c} {body[:80]}")

# well-known
c, body, _ = call("GET", "/.well-known/oauth-authorization-server")
check("AS metadata discovery", c == 200 and "token_endpoint" in j(body), f"{c}")

# --- buckets ---
c, body, _ = call("POST", "/v1/buckets", {"containerTag": TAG, "key": "work", "description": "Work stuff"}, auth=KEY)
check("create bucket", c == 200 and j(body).get("key") == "work", f"{c} {body[:80]}")
c, body, _ = call("POST", "/v1/profile/buckets", {"containerTag": TAG}, auth=KEY)
keys = [b.get("key") for b in j(body).get("buckets", [])]
check("list buckets (work + preferences)", c == 200 and "work" in keys and "preferences" in keys, f"{c} {keys}")

# --- analytics ---
c, body, _ = call("GET", "/v1/analytics/usage?period=30d", auth=KEY)
check("analytics usage", c == 200 and "usage" in j(body), f"{c}")
c, body, _ = call("GET", "/v1/analytics/logs", auth=KEY)
check("analytics logs", c == 200 and "logs" in j(body), f"{c}")

# --- inferred review ---
c, body, _ = call("GET", f"/v1/container-tags/{TAG}/inferred", auth=KEY)
check("inferred list", c == 200 and "total" in j(body), f"{c}")

# --- connectors ---
c, body, _ = call("GET", "/v1/connections", auth=KEY)
check("connections list", c == 200 and isinstance(j(body), list), f"{c}")
c, body, _ = call("POST", "/v1/connections/bogusprovider", {}, auth=KEY)
check("unknown provider -> 400", c == 400, f"{c}")
c, body, _ = call("POST", "/v1/connections/notion", {"containerTags": [TAG]}, auth=KEY)
# Either an auth_link (creds configured) or 400 (no client id) — both prove the path runs.
check("create oauth connection routes through connector", c in (200, 400), f"{c} {body[:80]}")

# --- memory router rejects private upstreams unless explicitly allowlisted ---
c, body, _ = call("POST", "/v1/router/http://127.0.0.1:9/v1/chat/completions",
                  {"model": "gpt", "messages": [{"role": "user", "content": "hi"}]},
                  headers={"x-memoricai-api-key": KEY, "x-mc-project": TAG})
check("memory router blocks private upstream by default", c == 400, f"{c}")

print()
passed = sum(1 for ok in results if ok)
print(f"==== Phase 2/3 E2E: {passed}/{len(results)} checks passed ====")
sys.exit(0 if passed == len(results) else 1)
