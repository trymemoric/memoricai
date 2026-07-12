# memoricai Python SDK

Stdlib-only client for the memoricai `/v1` HTTP API (Python 3.9+).

```python
from memoricai import Client

client = Client("http://localhost:7373", "mc_...")

doc = client.add_text("My name is Ada.", container_tag="mc_project_default")
client.wait_for_document(doc["id"])

res = client.search_memories("what is my name",
                             container_tag="mc_project_default", digest=True)
print(res["digest"])          # ready-to-inject, date-stamped context
print(client.profile("mc_project_default"))
```

Install from the repo: `pip install ./sdks/python`. Transient failures (429/5xx)
are retried with exponential backoff; API errors raise `MemoricaiError(status, message)`.
