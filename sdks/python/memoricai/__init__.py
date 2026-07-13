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
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Optional

__all__ = ["Client", "MemoricaiError", "ProcessingTimeout"]
__version__ = "0.1.3"


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

    def _request(self, method: str, path: str, body: Optional[dict] = None) -> Any:
        data = json.dumps(body).encode() if body is not None else None
        for attempt in range(self.max_retries + 1):
            req = urllib.request.Request(
                self.base_url + path, data=data, method=method
            )
            req.add_header("Authorization", "Bearer " + self.api_key)
            if data is not None:
                req.add_header("Content-Type", "application/json")
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                    payload = resp.read().decode()
                    return json.loads(payload) if payload else None
            except urllib.error.HTTPError as e:
                status = e.code
                text = e.read().decode()
                if status in (429, 500, 502, 503) and attempt < self.max_retries:
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

    # ---------------- documents ----------------

    def add_document(
        self,
        content: str,
        *,
        container_tag: Optional[str] = None,
        metadata: Optional[dict] = None,
        custom_id: Optional[str] = None,
        title: Optional[str] = None,
        content_type: Optional[str] = None,
        entity_context: Optional[str] = None,
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
                    "metadata": metadata,
                    "customId": custom_id,
                    "title": title,
                    "contentType": content_type,
                    "entityContext": entity_context,
                }
            ),
        )

    # Ergonomic alias.
    add_text = add_document

    def get_document(self, doc_id: str) -> dict:
        return self._request("GET", f"/v1/documents/{urllib.parse.quote(doc_id, safe='')}")

    def delete_document(self, doc_id: str) -> Any:
        return self._request("DELETE", f"/v1/documents/{urllib.parse.quote(doc_id, safe='')}")

    def list_documents(
        self,
        *,
        container_tags: Optional[list] = None,
        page: Optional[int] = None,
        limit: Optional[int] = None,
        status: Optional[str] = None,
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
                }
            ),
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
        rerank: bool = False,
        rewrite_query: bool = False,
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
                    "rerank": rerank,
                    "rewriteQuery": rewrite_query,
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
        metadata: Optional[dict] = None,
    ) -> dict:
        """PATCH /v1/memories — versioned update (appends a new version)."""
        return self._request(
            "PATCH",
            "/v1/memories",
            _drop_none(
                {"id": memory_id, "newContent": new_content, "metadata": metadata}
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
        threshold: float = 0.8,
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
