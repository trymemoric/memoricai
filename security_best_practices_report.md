# Production Security Review

## Executive summary

The reviewed Rust/Axum backend and TypeScript SDK are materially hardened for production. The original transaction, lease, connector, analytics, credential-storage, and SDK-test findings are resolved, and this final pass closes insecure production bootstrap, unbounded request-task creation, missing response headers, oversized public inputs, unlimited owner-key defaults, unbounded provider responses, and unsafe model URL handling.

No known critical or high-severity application-code finding remains in the reviewed scope. Production deployment still depends on the operational controls listed at the end of this report. The available security skill has no Rust/Axum-specific reference set, so the Rust review used established secure-default web-service, cryptographic-storage, OAuth, SSRF, and resource-bounding practices.

## Critical and high findings — resolved

### P1. Atomic index replacement and lease fencing

Impact: a failed or stale ingest worker could previously destroy or partially rewrite a live document index.

Chunk replacement, memory replacement, relation edges, bucket assignments, and the final document state now commit in one lease-locked transaction ([documents.rs](crates/memoricai-db/src/documents.rs:613), [pipeline.rs](crates/memoricai-engine/src/pipeline.rs:104)). Lease tokens and expiry are checked on stage changes, renewal, failure, and publication. Real PostgreSQL/pgvector tests cover rollback plus wrong and expired lease rejection ([engine_e2e.rs](crates/memoricai/tests/engine_e2e.rs:254)).

### P2. Credential storage failed open

Impact: missing, malformed, or unusable encryption configuration could store or return connector credentials as plaintext.

AES-256-GCM operations now fail closed, release/production mode requires a valid 32-byte key, encrypted values cannot be mistaken for bearer tokens, and existing connector tokens, cursors, metadata, OAuth codes, tokens, and client secrets are migrated ([crypto.rs](crates/memoricai-db/src/crypto.rs:59), [connections.rs](crates/memoricai-db/src/connections.rs:223)).

### P3. Implicit production owner credential

Impact: an empty production database previously caused the server to mint and print a full owner key automatically.

Release builds default to production mode, and production startup now fails until the explicit `memoricai key create` workflow has been completed ([config.rs](crates/memoricai/src/config.rs:32), [main.rs](crates/memoricai/src/main.rs:108)). The container image also runs as an unprivileged user with production mode enabled by default ([Dockerfile](Dockerfile:9)).

## Medium findings — resolved

### P4. Request-path resource exhaustion

Access logging no longer creates an unbounded detached task per request. HTTP execution is globally concurrency-limited, request bodies have a read timeout, public OAuth bodies are capped at 64 KiB, webhook bodies at 1 MiB, and analytics logs have bounded retention ([lib.rs](crates/memoricai-api/src/lib.rs:126), [main.rs](crates/memoricai/src/main.rs:229), [analytics.rs](crates/memoricai-db/src/analytics.rs:23)).

### P5. Missing cache and browser response protections

All API and MCP responses now receive `Cache-Control: no-store`, `Pragma: no-cache`, `X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`, a restrictive CSP, and a restrictive Permissions Policy ([lib.rs](crates/memoricai-api/src/lib.rs:162)). This protects the OAuth consent form from caching and framing without assuming that the application directly terminates TLS.

### P6. OAuth and bearer input hardening

OAuth authorize, consent, token, registration, callback, and webhook inputs now have explicit size ceilings. Bearer tokens are centrally length/control-character validated. OAuth authorization codes and other verification-only credentials are stored as hashes. Provider OAuth failures no longer reflect arbitrary upstream response bodies.

### P7. Untrusted provider response bounds

Connector downloads and error bodies are streamed under hard limits. Model, embedding, reranking, transcription, and vision JSON responses are capped at 16 MiB, and configured model URLs reject embedded credentials, queries, fragments, and non-HTTP schemes ([models/lib.rs](crates/memoricai-models/src/lib.rs:22), [models/lib.rs](crates/memoricai-models/src/lib.rs:70)).

### P8. Connector completeness and deletion safety

Notion recursively paginates child block trees ([notion.rs](crates/memoricai-connectors/src/notion.rs:70)). Drive, Gmail, Notion, and OneDrive use incremental checkpoints with stale-cursor recovery. Complete empty enumerations delete stale documents, and reconciliation repairs memory graphs transactionally without a permanent percentage skip ([documents.rs](crates/memoricai-db/src/documents.rs:543)).

### P9. Analytics coverage

Authentication failures, public OAuth/discovery traffic, webhook failures, router failures, and successful custom-auth paths now pass through request analytics. Anonymous traffic is stored with nullable identity rather than omitted.

## Remaining operational considerations

1. **TLS and network policy:** terminate TLS at a trusted reverse proxy/load balancer, use TLS for remote PostgreSQL and model providers, and restrict database/model network access. HSTS is intentionally not enabled by the application because TLS termination is deployment-specific.
2. **Secret management:** store `MEMORICAI_ENCRYPTION_KEY`, model/provider keys, database credentials, webhook secrets, and printed owner keys in a secret manager. Back up the encryption key separately; losing it makes encrypted connector credentials unrecoverable.
3. **Multi-replica rate limits:** API-key and dynamic-registration fixed-window counters are process-local. Multi-replica deployments should enforce an additional shared rate limit at the ingress/API gateway.
4. **Dependency maintenance:** `cargo audit` reports no known vulnerability, but flags the unmaintained transitive `paste 1.0.15` crate through optional `fastembed`. Track the upstream replacement; this is a maintenance warning, not a vulnerability advisory.
5. **External verification:** live provider connector calls and the live-server SDK test still require deployment-owned credentials and endpoints. Unit, build, and real PostgreSQL/pgvector validation do not replace staging tests against those external systems.

## Production checklist

- Use a release binary or set `MEMORICAI_ENV=production`.
- Set a stable 32-byte `MEMORICAI_ENCRYPTION_KEY` before migrations or startup.
- Run `memoricai key create` explicitly before `memoricai serve`.
- Set canonical HTTPS `MEMORICAI_BASE_URL` when OAuth/connectors are enabled.
- Keep router and connector private-origin allowlists minimal.
- Put the service behind TLS, ingress rate limiting, request-size limits, and network ACLs.
- Monitor authentication failures, 429/5xx rates, sync failures, lease conflicts, database capacity, and analytics retention jobs.
