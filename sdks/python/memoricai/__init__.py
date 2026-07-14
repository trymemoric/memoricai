"""Python SDK for the memoricai /v1 HTTP API. Stdlib only.

    from memoricai import Client

    client = Client("http://localhost:7373", "mc_...")
    doc = client.add_text("My name is Ada.", container_tag="mc_project_default")
    client.wait_for_document(doc["id"])
    res = client.search_memories("what is my name",
                                 container_tag="mc_project_default", digest=True)
    print(res.get("digest"))
"""

from __future__ import annotations

import json
import mimetypes
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from typing import Any, Optional

__all__ = ["Client", "MemoricaiError", "ProcessingTimeout"]
__version__ = "0.3.2"


class MemoricaiError(Exception):
    """Raised for non-2xx API responses."""

    def __init__(self, status: int, message: str):
        super().__init__(f"api error {status}: {message}")
        self.status = status
        self.message = message


class ProcessingTimeout(MemoricaiError):
    """Raised when wait_for_document exceeds its timeout."""

    def __init__(self, doc_id: str):
        super().__init__(408, f"timed out waiting for document {doc_id}")


def _drop_none(d: dict) -> dict:
    return {k: v for k, v in d.items() if v is not None}


def _query_value(value: Any) -> Any:
    if isinstance(value, bool):
        return "true" if value else "false"
    return value


class Client:
    """Client for a memoricai server's /v1 API.

    Retries transient failures (429/5xx) with exponential backoff.
    """

    def __init__(
        self,
        base_url: str,
        api_key: str,
        timeout: float = 120.0,
        max_retries: int = 4,
    ):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.timeout = timeout
        self.max_retries = max_retries

    # ---------------- transport ----------------

    def _request(
        self,
        method: str,
        path: str,
        body: Optional[Any] = None,
        *,
        query: Optional[dict] = None,
        headers: Optional[dict] = None,
        raw_data: Optional[bytes] = None,
    ) -> Any:
        """Send an API request.

        This is intentionally public-by-convention (despite the leading
        underscore) as a forward-compatibility escape hatch for newly added
        engine endpoints. Prefer the typed convenience methods below.
        """
        if body is not None and raw_data is not None:
            raise ValueError("body and raw_data are mutually exclusive")
        data = json.dumps(body).encode() if body is not None else raw_data
        url = self.base_url + path
        if query:
            encoded = urllib.parse.urlencode(
                {
                    key: _query_value(value)
                    for key, value in query.items()
                    if value is not None
                },
                doseq=True,
            )
            if encoded:
                url += ("&" if "?" in url else "?") + encoded
        for attempt in range(self.max_retries + 1):
            req = urllib.request.Request(url, data=data, method=method)
            supplied_headers = headers or {}
            if not any(key.lower() == "authorization" for key in supplied_headers):
                req.add_header("Authorization", "Bearer " + self.api_key)
            if body is not None and not any(
                key.lower() == "content-type" for key in supplied_headers
            ):
                req.add_header("Content-Type", "application/json")
            for key, value in supplied_headers.items():
                req.add_header(key, value)
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                    payload = resp.read()
                    if not payload:
                        return None
                    content_type = resp.headers.get("content-type", "").lower()
                    if "json" in content_type:
                        return json.loads(payload.decode())
                    return payload
            except urllib.error.HTTPError as e:
                status = e.code
                text = e.read().decode()
                if (status == 429 or 500 <= status <= 599) and attempt < self.max_retries:
                    time.sleep(2**attempt)
                    continue
                try:
                    parsed = json.loads(text)
                    message = parsed.get("message") or parsed.get("error") or text
                except Exception:
                    message = text
                raise MemoricaiError(status, message) from None
        raise MemoricaiError(0, "retries exhausted")

    def health(self) -> dict:
        return self._request("GET", "/health")

    def request(
        self,
        method: str,
        path: str,
        body: Optional[Any] = None,
        **options: Any,
    ) -> Any:
        """Low-level JSON request for forward-compatible engine access."""
        return self._request(method, path, body, **options)

    def openapi(self) -> dict:
        """GET /v1/openapi — engine discovery document."""
        return self._request("GET", "/v1/openapi")

    def oauth_metadata(self) -> dict:
        return self._request("GET", "/.well-known/oauth-authorization-server")

    def register_oauth_client(
        self,
        redirect_uris: list,
        *,
        client_name: Optional[str] = None,
        grant_types: Optional[list] = None,
        token_endpoint_auth_method: Optional[str] = None,
    ) -> dict:
        return self._request(
            "POST",
            "/api/auth/oauth2/register",
            _drop_none(
                {
                    "redirect_uris": redirect_uris,
                    "client_name": client_name,
                    "grant_types": grant_types,
                    "token_endpoint_auth_method": token_endpoint_auth_method,
                }
            ),
        )

    def exchange_oauth_token(
        self,
        grant_type: str,
        client_id: str,
        *,
        client_secret: Optional[str] = None,
        code: Optional[str] = None,
        redirect_uri: Optional[str] = None,
        code_verifier: Optional[str] = None,
        refresh_token: Optional[str] = None,
    ) -> dict:
        form = urllib.parse.urlencode(
            _drop_none(
                {
                    "grant_type": grant_type,
                    "client_id": client_id,
                    "client_secret": client_secret,
                    "code": code,
                    "redirect_uri": redirect_uri,
                    "code_verifier": code_verifier,
                    "refresh_token": refresh_token,
                }
            )
        ).encode()
        return self._request(
            "POST",
            "/api/auth/oauth2/token",
            headers={"Content-Type": "application/x-www-form-urlencoded"},
            raw_data=form,
        )

    # ---------------- documents ----------------

    def add_document(
        self,
        content: str,
        *,
        container_tag: Optional[str] = None,
        container_tags: Optional[list] = None,
        metadata: Optional[dict] = None,
        custom_id: Optional[str] = None,
        title: Optional[str] = None,
        content_type: Optional[str] = None,
        entity_context: Optional[str] = None,
        raw: Optional[str] = None,
    ) -> dict:
        """POST /v1/documents — returns {id, status:"queued"} instantly;
        extraction/embedding/indexing happen in the background."""
        return self._request(
            "POST",
            "/v1/documents",
            _drop_none(
                {
                    "content": content,
                    "containerTag": container_tag,
                    "containerTags": container_tags,
                    "metadata": metadata,
                    "customId": custom_id,
                    "title": title,
                    "contentType": content_type,
                    "entityContext": entity_context,
                    "raw": raw,
                }
            ),
        )

    def add_documents(
        self,
        documents: list,
        *,
        container_tag: Optional[str] = None,
        entity_context: Optional[str] = None,
        metadata: Optional[dict] = None,
    ) -> dict:
        """POST /v1/documents/batch — enqueue up to 600 documents."""
        return self._request(
            "POST",
            "/v1/documents/batch",
            _drop_none(
                {
                    "documents": documents,
                    "containerTag": container_tag,
                    "entityContext": entity_context,
                    "metadata": metadata,
                }
            ),
        )

    def upload_file(
        self,
        content: bytes,
        filename: str,
        *,
        container_tags: Optional[list] = None,
        metadata: Optional[dict] = None,
        content_type: Optional[str] = None,
        container_tag: Optional[str] = None,
    ) -> dict:
        """POST /v1/documents/file — upload PDF, image, audio/video, or text bytes."""
        if not isinstance(content, (bytes, bytearray, memoryview)):
            raise TypeError("content must be bytes-like")
        if any(character in filename for character in ("\r", "\n", '"')):
            raise ValueError("filename contains an unsafe header character")
        boundary = "----memoricai-" + uuid.uuid4().hex
        parts = []

        def add_field(name: str, value: str) -> None:
            parts.extend(
                [
                    f"--{boundary}\r\n".encode(),
                    f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode(),
                    value.encode(),
                    b"\r\n",
                ]
            )

        if container_tag is not None:
            add_field("containerTag", container_tag)
        for tag in container_tags or []:
            add_field("containerTags", tag)
        if metadata is not None:
            add_field("metadata", json.dumps(metadata))
        media_type = content_type or mimetypes.guess_type(filename)[0] or "application/octet-stream"
        parts.extend(
            [
                f"--{boundary}\r\n".encode(),
                f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'.encode(),
                f"Content-Type: {media_type}\r\n\r\n".encode(),
                bytes(content),
                b"\r\n",
                f"--{boundary}--\r\n".encode(),
            ]
        )
        return self._request(
            "POST",
            "/v1/documents/file",
            headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
            raw_data=b"".join(parts),
        )

    # Ergonomic alias.
    add_text = add_document

    def get_document(self, doc_id: str) -> dict:
        return self._request("GET", f"/v1/documents/{urllib.parse.quote(doc_id, safe='')}")

    def delete_document(self, doc_id: str) -> Any:
        return self._request("DELETE", f"/v1/documents/{urllib.parse.quote(doc_id, safe='')}")

    def patch_document(
        self,
        doc_id: str,
        *,
        content: Optional[str] = None,
        metadata: Optional[dict] = None,
    ) -> dict:
        return self._request(
            "PATCH",
            f"/v1/documents/{urllib.parse.quote(doc_id, safe='')}",
            _drop_none({"content": content, "metadata": metadata}),
        )

    def list_documents(
        self,
        *,
        container_tags: Optional[list] = None,
        page: Optional[int] = None,
        limit: Optional[int] = None,
        status: Optional[str] = None,
        sort: Optional[str] = None,
        order: Optional[str] = None,
    ) -> dict:
        return self._request(
            "POST",
            "/v1/documents/list",
            _drop_none(
                {
                    "containerTags": container_tags,
                    "page": page,
                    "limit": limit,
                    "status": status,
                    "sort": sort,
                    "order": order,
                }
            ),
        )

    def list_documents_with_memories(
        self,
        *,
        container_tags: Optional[list] = None,
        page: Optional[int] = None,
        limit: Optional[int] = None,
    ) -> dict:
        return self._request(
            "POST",
            "/v1/documents/documents",
            _drop_none(
                {"containerTags": container_tags, "page": page, "limit": limit}
            ),
        )

    def list_processing_documents(self) -> dict:
        return self._request("GET", "/v1/documents/processing")

    def bulk_delete_documents(
        self,
        *,
        ids: Optional[list] = None,
        container_tags: Optional[list] = None,
    ) -> dict:
        return self._request(
            "DELETE",
            "/v1/documents/bulk",
            _drop_none({"ids": ids, "containerTags": container_tags}),
        )

    def search_documents(
        self,
        q: str,
        *,
        container_tags: Optional[list] = None,
        limit: int = 10,
        chunk_threshold: float = 0.5,
        document_threshold: float = 0.5,
        include_full_docs: bool = False,
        include_summary: bool = False,
        rerank: bool = False,
        rewrite_query: bool = False,
        doc_id: Optional[str] = None,
        filters: Optional[dict] = None,
    ) -> dict:
        """POST /v1/documents/search — chunk-level RAG over documents."""
        return self._request(
            "POST",
            "/v1/documents/search",
            _drop_none(
                {
                    "q": q,
                    "containerTags": container_tags,
                    "limit": limit,
                    "chunkThreshold": chunk_threshold,
                    "documentThreshold": document_threshold,
                    "includeFullDocs": include_full_docs,
                    "includeSummary": include_summary,
                    "rerank": rerank,
                    "rewriteQuery": rewrite_query,
                    "docId": doc_id,
                    "filters": filters,
                }
            ),
        )

    def wait_for_document(self, doc_id: str, timeout: float = 120.0) -> dict:
        """Poll GET /v1/documents/{id} until done (raises on failed/timeout)."""
        deadline = time.time() + timeout
        while True:
            doc = self.get_document(doc_id)
            status = doc.get("status")
            if status == "done":
                return doc
            if status == "failed":
                raise MemoricaiError(500, f"document {doc_id} failed processing")
            if time.time() >= deadline:
                raise ProcessingTimeout(doc_id)
            time.sleep(0.4)

    # ---------------- search / profile ----------------

    def search_memories(
        self,
        q: str,
        *,
        container_tag: Optional[str] = None,
        search_mode: str = "hybrid",
        limit: int = 10,
        threshold: float = 0.5,
        digest: bool = False,
        rerank: bool = False,
        rewrite_query: bool = False,
        include: Optional[dict] = None,
        filters: Optional[dict] = None,
    ) -> dict:
        """POST /v1/search — memory-graph search. digest=True adds a compact,
        date-stamped context digest to the response."""
        return self._request(
            "POST",
            "/v1/search",
            _drop_none(
                {
                    "q": q,
                    "containerTag": container_tag,
                    "searchMode": search_mode,
                    "limit": limit,
                    "threshold": threshold,
                    "digest": digest,
                    "rerank": rerank,
                    "rewriteQuery": rewrite_query,
                    "include": include,
                    "filters": filters,
                }
            ),
        )

    def build_context(
        self,
        q: str,
        *,
        container_tag: Optional[str] = None,
        mode: str = "auto",
        budget_tokens: int = 12_000,
        max_sources: int = 8,
        threshold: float = 0.5,
        rewrite_query: bool = False,
        filters: Optional[dict] = None,
        include_digest: bool = True,
    ) -> dict:
        """POST /v1/context — bounded, source-aware context ready for an LLM prompt."""
        return self._request(
            "POST",
            "/v1/context",
            _drop_none(
                {
                    "q": q,
                    "containerTag": container_tag,
                    "mode": mode,
                    "budgetTokens": budget_tokens,
                    "maxSources": max_sources,
                    "threshold": threshold,
                    "rewriteQuery": rewrite_query,
                    "filters": filters,
                    "includeDigest": include_digest,
                }
            ),
        )

    def profile(
        self,
        container_tag: str,
        *,
        q: Optional[str] = None,
        threshold: Optional[float] = None,
        include: Optional[list] = None,
        buckets: Optional[list] = None,
        filters: Optional[dict] = None,
    ) -> dict:
        """POST /v1/profile — static/dynamic/bucketed user profile."""
        return self._request(
            "POST",
            "/v1/profile",
            _drop_none(
                {
                    "containerTag": container_tag,
                    "q": q,
                    "threshold": threshold,
                    "include": include,
                    "buckets": buckets,
                    "filters": filters,
                }
            ),
        )

    # ---------------- memories ----------------

    def create_memories(self, container_tag: str, memories: list) -> dict:
        """POST /v1/memories — create memories directly (no extraction).
        memories: [{"content": str, "isStatic": bool, "metadata": dict|None}]."""
        return self._request(
            "POST",
            "/v1/memories",
            {"containerTag": container_tag, "memories": memories},
        )

    def patch_memory(
        self,
        new_content: str,
        *,
        memory_id: Optional[str] = None,
        content: Optional[str] = None,
        metadata: Optional[dict] = None,
    ) -> dict:
        """PATCH /v1/memories — versioned update (appends a new version)."""
        return self._request(
            "PATCH",
            "/v1/memories",
            _drop_none(
                {
                    "id": memory_id,
                    "content": content,
                    "newContent": new_content,
                    "metadata": metadata,
                }
            ),
        )

    def forget_memory(
        self,
        container_tag: str,
        *,
        memory_id: Optional[str] = None,
        content: Optional[str] = None,
        reason: Optional[str] = None,
    ) -> dict:
        """DELETE /v1/memories — forget one memory by id or exact content."""
        return self._request(
            "DELETE",
            "/v1/memories",
            _drop_none(
                {
                    "containerTag": container_tag,
                    "id": memory_id,
                    "content": content,
                    "reason": reason,
                }
            ),
        )

    def forget_matching(
        self,
        container_tag: str,
        query: str,
        *,
        threshold: float = 0.5,
        max_forget: int = 100,
        dry_run: bool = True,
        reason: Optional[str] = None,
    ) -> dict:
        """POST /v1/memories/forget-matching — semantic bulk forget.
        Defaults to dry_run=True; pass dry_run=False to actually forget."""
        return self._request(
            "POST",
            "/v1/memories/forget-matching",
            _drop_none(
                {
                    "containerTag": container_tag,
                    "query": query,
                    "threshold": threshold,
                    "maxForget": max_forget,
                    "dryRun": dry_run,
                    "reason": reason,
                }
            ),
        )

    # ---------------- projects / tags ----------------

    def list_projects(self) -> dict:
        return self._request("GET", "/v1/projects")

    list_container_tags = list_projects

    def create_project(self, name: str, *, emoji: Optional[str] = None) -> dict:
        return self._request("POST", "/v1/projects", _drop_none({"name": name, "emoji": emoji}))

    def delete_project(
        self,
        project_id: str,
        *,
        action: str = "delete",
        target_project_id: Optional[str] = None,
    ) -> dict:
        return self._request(
            "DELETE",
            f"/v1/projects/{urllib.parse.quote(project_id, safe='')}",
            _drop_none({"action": action, "targetProjectId": target_project_id}),
        )

    def update_container_tag(
        self,
        tag: str,
        *,
        name: Optional[str] = None,
        entity_context: Optional[str] = None,
    ) -> dict:
        return self._request(
            "PATCH",
            f"/v1/container-tags/{urllib.parse.quote(tag, safe='')}",
            _drop_none({"name": name, "entityContext": entity_context}),
        )

    def delete_container_tag(self, tag: str) -> dict:
        return self._request(
            "DELETE", f"/v1/container-tags/{urllib.parse.quote(tag, safe='')}"
        )

    # ---------------- settings / auth ----------------

    def get_settings(self) -> dict:
        return self._request("GET", "/v1/settings")

    def update_settings(
        self,
        *,
        should_llm_filter: Optional[bool] = None,
        filter_prompt: Optional[str] = None,
        categories: Optional[list] = None,
        include_items: Optional[list] = None,
        exclude_items: Optional[list] = None,
        chunk_size: Optional[int] = None,
    ) -> dict:
        return self._request(
            "PATCH",
            "/v1/settings",
            _drop_none(
                {
                    "shouldLlmFilter": should_llm_filter,
                    "filterPrompt": filter_prompt,
                    "categories": categories,
                    "includeItems": include_items,
                    "excludeItems": exclude_items,
                    "chunkSize": chunk_size,
                }
            ),
        )

    def reset_settings(self, confirmation: str = "RESET") -> dict:
        return self._request(
            "POST", "/v1/settings/reset", {"confirmation": confirmation}
        )

    def session(self) -> dict:
        return self._request("GET", "/v1/session")

    def create_scoped_key(
        self,
        container_tag: str,
        *,
        name: Optional[str] = None,
        expires_in_days: Optional[int] = None,
        rate_limit_max: Optional[int] = None,
        rate_limit_time_window: Optional[int] = None,
    ) -> dict:
        return self._request(
            "POST",
            "/v1/auth/scoped-key",
            _drop_none(
                {
                    "containerTag": container_tag,
                    "name": name,
                    "expiresInDays": expires_in_days,
                    "rateLimitMax": rate_limit_max,
                    "rateLimitTimeWindow": rate_limit_time_window,
                }
            ),
        )

    def revoke_scoped_key(self, key_id: str) -> dict:
        return self._request(
            "DELETE", f"/v1/auth/scoped-key/{urllib.parse.quote(key_id, safe='')}"
        )

    # ---------------- profile buckets / inferred memories ----------------

    def list_profile_buckets(self, *, container_tag: Optional[str] = None) -> dict:
        return self._request(
            "POST", "/v1/profile/buckets", _drop_none({"containerTag": container_tag})
        )

    def create_profile_bucket(
        self, key: str, description: str, *, container_tag: Optional[str] = None
    ) -> dict:
        return self._request(
            "POST",
            "/v1/buckets",
            _drop_none(
                {"containerTag": container_tag, "key": key, "description": description}
            ),
        )

    def list_inferred_memories(self, tag: str) -> dict:
        return self._request(
            "GET", f"/v1/container-tags/{urllib.parse.quote(tag, safe='')}/inferred"
        )

    def review_inferred_memory(self, tag: str, memory_id: str, action: str) -> dict:
        return self._request(
            "POST",
            f"/v1/container-tags/{urllib.parse.quote(tag, safe='')}/inferred/"
            f"{urllib.parse.quote(memory_id, safe='')}/review",
            {"action": action},
        )

    # ---------------- analytics ----------------

    def _analytics(
        self,
        resource: str,
        *,
        period: Optional[str] = None,
        page: Optional[int] = None,
        limit: Optional[int] = None,
    ) -> dict:
        return self._request(
            "GET",
            f"/v1/analytics/{resource}",
            query={"period": period, "page": page, "limit": limit},
        )

    def analytics_usage(self, **query: Any) -> dict:
        return self._analytics("usage", **query)

    def analytics_errors(self, **query: Any) -> dict:
        return self._analytics("errors", **query)

    def analytics_logs(self, **query: Any) -> dict:
        return self._analytics("logs", **query)

    def analytics_memory(self) -> dict:
        return self._analytics("memory")

    def analytics_chat(self) -> dict:
        return self._analytics("chat")

    # ---------------- connections ----------------

    def list_connections(
        self,
        *,
        container_tags: Optional[list] = None,
        provider: Optional[str] = None,
    ) -> list:
        if container_tags is None and provider is None:
            return self._request("GET", "/v1/connections")
        return self._request(
            "POST",
            "/v1/connections/list",
            _drop_none({"containerTags": container_tags, "provider": provider}),
        )

    def create_connection(
        self,
        provider: str,
        *,
        redirect_url: Optional[str] = None,
        container_tags: Optional[list] = None,
        document_limit: Optional[int] = None,
        metadata: Optional[dict] = None,
    ) -> dict:
        return self._request(
            "POST",
            f"/v1/connections/{urllib.parse.quote(provider, safe='')}",
            _drop_none(
                {
                    "redirectUrl": redirect_url,
                    "containerTags": container_tags,
                    "documentLimit": document_limit,
                    "metadata": metadata,
                }
            ),
        )

    def get_connection(self, connection_id: str) -> dict:
        return self._request(
            "GET", f"/v1/connections/{urllib.parse.quote(connection_id, safe='')}"
        )

    def delete_connection(
        self, connection_id_or_provider: str, *, delete_documents: bool = True
    ) -> dict:
        return self._request(
            "DELETE",
            f"/v1/connections/{urllib.parse.quote(connection_id_or_provider, safe='')}",
            query={"deleteDocuments": delete_documents},
        )

    def import_connection(self, connection_id_or_provider: str) -> dict:
        return self._request(
            "POST",
            f"/v1/connections/{urllib.parse.quote(connection_id_or_provider, safe='')}/import",
            {},
        )

    def connection_sync_runs(self, connection_id: str) -> list:
        return self._request(
            "GET",
            f"/v1/connections/{urllib.parse.quote(connection_id, safe='')}/sync-runs",
        )

    def connection_resources(
        self, connection_id: str, *, page: int = 1, per_page: int = 30
    ) -> dict:
        return self._request(
            "GET",
            f"/v1/connections/{urllib.parse.quote(connection_id, safe='')}/resources",
            query={"page": page, "perPage": per_page},
        )

    def configure_connection(self, connection_id: str, configuration: dict) -> dict:
        return self._request(
            "POST",
            f"/v1/connections/{urllib.parse.quote(connection_id, safe='')}/configure",
            configuration,
        )

    # ---------------- memory router / MCP OAuth helpers ----------------

    def router_request(
        self,
        upstream_url: str,
        request_body: dict,
        upstream_api_key: str,
        *,
        container_tag: Optional[str] = None,
    ) -> Any:
        """Proxy an OpenAI-compatible request through the memory router.

        JSON upstream responses are decoded; streaming/non-JSON responses are
        returned as bytes.
        """
        headers = {
            "Authorization": "Bearer " + upstream_api_key,
            "x-memoricai-api-key": self.api_key,
        }
        if container_tag is not None:
            headers["x-mc-project"] = container_tag
        target = urllib.parse.quote(
            upstream_url, safe=":/@!$&'()*+,;=-._~"
        )
        return self._request("POST", "/v1/router/" + target, request_body, headers=headers)

    def mcp_session_with_key(self) -> dict:
        return self._request("GET", "/v1/mcp/session-with-key")

    def connect_mcp_scope(self, body: dict) -> dict:
        return self._request("POST", "/v1/mcp/connect-scope", body)

    def provision(self, org_name: str, email: str) -> dict:
        """POST /v1/admin/provision using this client's key as the provision key."""
        return self._request(
            "POST", "/v1/admin/provision", {"orgName": org_name, "email": email}
        )
