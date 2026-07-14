# Python SDK (`memoricai`)

Stdlib-only client for the [`/v1` HTTP API](api.md). Python 3.9+, no
dependencies. Methods take keyword arguments in snake_case and translate to the
API's camelCase; all methods return the decoded JSON response as a `dict`.

```bash
pip install memoricai
```

## Quickstart

```python
from memoricai import Client

client = Client("http://localhost:7373", "mc_...")

doc = client.add_text("My name is Ada.", container_tag="mc_project_default")
client.wait_for_document(doc["id"])

res = client.search_memories("what is my name",
                             container_tag="mc_project_default", digest=True)
print(res["digest"])            # ready-to-inject, date-stamped context
print(client.profile("mc_project_default"))
```

## Construction & behavior

```python
Client(base_url, api_key, timeout=120.0, max_retries=4)
```

- Transient failures (**429, 500, 502, 503**) are retried with exponential
  backoff (1 s, 2 s, 4 s, …) up to `max_retries` times.
- Other non-2xx responses raise **`MemoricaiError(status, message)`** with the
  message from the server's error envelope.
- `wait_for_document` raises **`ProcessingTimeout`** (a `MemoricaiError`
  subclass) past its deadline, and `MemoricaiError(500, …)` if the document
  reaches `failed`.

## Methods

### Documents

```python
client.add_document(content, *, container_tag=None, metadata=None,
                    custom_id=None, title=None, content_type=None,
                    entity_context=None) -> dict        # POST /v1/documents
client.add_text(...)                                    # alias of add_document
client.get_document(doc_id) -> dict                     # GET /v1/documents/{id}
client.delete_document(doc_id)                          # DELETE /v1/documents/{id}
client.list_documents(*, container_tags=None, page=None,
                      limit=None, status=None) -> dict  # POST /v1/documents/list
client.wait_for_document(doc_id, timeout=120.0) -> dict # poll until "done"
```

### Search

```python
client.search_documents(q, *, container_tags=None, limit=10,
                        chunk_threshold=0.5, document_threshold=0.5,
                        include_full_docs=False, rerank=False,
                        rewrite_query=False) -> dict     # POST /v1/documents/search

client.search_memories(q, *, container_tag=None, search_mode="hybrid",
                       limit=10, threshold=0.5, digest=False,
                       rerank=False, rewrite_query=False,
                       include=None) -> dict             # POST /v1/search

client.build_context(q, *, container_tag=None, mode="auto",
                     budget_tokens=12000, max_sources=8, threshold=0.5,
                     rewrite_query=False, filters=None,
                     include_digest=True) -> dict       # POST /v1/context
```

`search_memories(..., digest=True)` adds `"digest"` to the response, the
compact, date-stamped context block described in the [API docs](api.md#post-v1search).
`include` takes the raw API shape, e.g.
`{"documents": True, "relatedMemories": True}`.
`build_context(...)` returns a bounded context packet plus per-source evidence
and explicit budget/source-limit omission diagnostics.

### Profile

```python
client.profile(container_tag, *, q=None, threshold=None,
               include=None, buckets=None) -> dict       # POST /v1/profile
```

### Memories

```python
client.create_memories(container_tag,
    [{"content": "Prefers dark mode.", "isStatic": True}]) -> dict
                                                          # POST /v1/memories
client.patch_memory(new_content, *, memory_id=None, metadata=None) -> dict
                                                          # PATCH /v1/memories
client.forget_memory(container_tag, *, memory_id=None,
                     content=None, reason=None) -> dict   # DELETE /v1/memories
client.forget_matching(container_tag, query, *, threshold=0.8,
                       max_forget=100, dry_run=True, reason=None) -> dict
                                                          # POST /v1/memories/forget-matching
```

Note: `forget_matching` defaults to `dry_run=True` **client-side** (safer than
the server default of `false`), pass `dry_run=False` to actually forget.

### Misc

```python
client.health() -> dict   # GET /health
```
