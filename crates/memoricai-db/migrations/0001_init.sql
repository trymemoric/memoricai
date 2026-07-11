-- memoricai initial schema. pgvector columns are dimensionless (`vector`) so the
-- embedding dimension stays configurable; Phase 1 uses exact scan, ANN indexes
-- are a Phase 2 per-deployment migration once the dimension is fixed.

CREATE EXTENSION IF NOT EXISTS vector;

-- ---------------- identity ----------------

CREATE TABLE IF NOT EXISTS users (
    id    text PRIMARY KEY,
    email text NOT NULL UNIQUE,
    name  text
);

CREATE TABLE IF NOT EXISTS organizations (
    id       text PRIMARY KEY,
    name     text NOT NULL,
    metadata jsonb NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS members (
    user_id        text NOT NULL,
    org_id         text NOT NULL,
    role           text NOT NULL DEFAULT 'member',
    access_type    text NOT NULL DEFAULT 'full',
    container_tags text[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (user_id, org_id)
);

CREATE TABLE IF NOT EXISTS api_keys (
    id                   text PRIMARY KEY,
    key_hash             text NOT NULL,
    prefix               text NOT NULL,
    last4                text NOT NULL,
    org_id               text NOT NULL,
    user_id              text,
    name                 text NOT NULL,
    key_type             text NOT NULL DEFAULT 'org',
    container_tag        text,
    allowed_endpoints    text[],
    rate_limit_max       int NOT NULL DEFAULT 500,
    rate_limit_window_ms bigint NOT NULL DEFAULT 60000,
    expires_at           timestamptz,
    revoked              boolean NOT NULL DEFAULT false,
    created_at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS api_keys_prefix_idx ON api_keys (prefix);

-- ---------------- spaces / projects ----------------

CREATE TABLE IF NOT EXISTS spaces (
    id              text PRIMARY KEY,
    name            text NOT NULL,
    description     text,
    org_id          text NOT NULL,
    owner_id        text,
    container_tag   text NOT NULL,
    entity_context  text,
    emoji           text,
    visibility      text NOT NULL DEFAULT 'private',
    is_experimental boolean NOT NULL DEFAULT false,
    metadata        jsonb NOT NULL DEFAULT '{}',
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, container_tag)
);

-- ---------------- documents ----------------

CREATE TABLE IF NOT EXISTS documents (
    id                text PRIMARY KEY,
    custom_id         text,
    content_hash      text,
    org_id            text NOT NULL,
    user_id           text,
    connection_id     text,
    title             text,
    summary           text,
    content           text,
    raw               text,
    url               text,
    source            text,
    doc_type          text NOT NULL DEFAULT 'text',
    status            text NOT NULL DEFAULT 'queued',
    metadata          jsonb NOT NULL DEFAULT '{}',
    container_tags    text[] NOT NULL DEFAULT '{}',
    token_count       bigint,
    chunk_count       bigint DEFAULT 0,
    summary_embedding vector,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS documents_org_custom_id_idx
    ON documents (org_id, custom_id) WHERE custom_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS documents_org_idx ON documents (org_id);
CREATE INDEX IF NOT EXISTS documents_container_tags_idx ON documents USING gin (container_tags);
CREATE INDEX IF NOT EXISTS documents_status_idx ON documents (status);

-- ---------------- memories ----------------

CREATE TABLE IF NOT EXISTS memories (
    id                  text PRIMARY KEY,
    custom_id           text,
    document_id         text,
    org_id              text NOT NULL,
    user_id             text,
    memory              text NOT NULL,
    summary             text,
    mem_type            text,
    space_container_tag text NOT NULL,
    embedding           vector,
    version             int NOT NULL DEFAULT 1,
    is_latest           boolean NOT NULL DEFAULT true,
    parent_memory_id    text,
    root_memory_id      text,
    relation            text,
    source_count        int NOT NULL DEFAULT 1,
    is_static           boolean NOT NULL DEFAULT false,
    is_inference        boolean NOT NULL DEFAULT false,
    review_status       text,
    is_forgotten        boolean NOT NULL DEFAULT false,
    forget_reason       text,
    forget_after        timestamptz,
    forget_batch_id     text,
    metadata            jsonb NOT NULL DEFAULT '{}',
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS memories_scope_idx ON memories (org_id, space_container_tag);
CREATE INDEX IF NOT EXISTS memories_document_idx ON memories (document_id);
CREATE INDEX IF NOT EXISTS memories_root_idx ON memories (root_memory_id);
CREATE UNIQUE INDEX IF NOT EXISTS memories_latest_per_root_idx
    ON memories (root_memory_id) WHERE is_latest AND root_memory_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS memory_relations (
    source_memory_id text NOT NULL,
    target_memory_id text NOT NULL,
    relation         text NOT NULL,
    PRIMARY KEY (source_memory_id, target_memory_id)
);
CREATE INDEX IF NOT EXISTS memory_relations_target_idx ON memory_relations (target_memory_id);

-- ---------------- chunks ----------------

CREATE TABLE IF NOT EXISTS chunks (
    id                  text PRIMARY KEY,
    document_id         text NOT NULL,
    org_id              text NOT NULL,
    space_container_tag text NOT NULL,
    content             text NOT NULL,
    chunk_type          text NOT NULL DEFAULT 'text',
    position            int NOT NULL DEFAULT 0,
    embedding           vector,
    metadata            jsonb NOT NULL DEFAULT '{}',
    created_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS chunks_document_idx ON chunks (document_id);
CREATE INDEX IF NOT EXISTS chunks_scope_idx ON chunks (org_id, space_container_tag);

-- ---------------- settings ----------------

CREATE TABLE IF NOT EXISTS organization_settings (
    org_id           text PRIMARY KEY,
    should_llm_filter boolean NOT NULL DEFAULT false,
    filter_prompt     text,
    categories        text[],
    include_items     text[],
    exclude_items     text[],
    chunk_size        int NOT NULL DEFAULT -1,
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- ---------------- analytics ----------------

CREATE TABLE IF NOT EXISTS api_requests (
    id          text PRIMARY KEY,
    type        text NOT NULL,
    org_id      text,
    user_id     text,
    key_id      text,
    status_code int,
    duration    bigint,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS api_requests_org_idx ON api_requests (org_id, created_at);

-- ---------------- connectors (Phase 3 scaffold) ----------------

CREATE TABLE IF NOT EXISTS connections (
    id             text PRIMARY KEY,
    provider       text NOT NULL,
    org_id         text NOT NULL,
    user_id        text,
    email          text,
    document_limit int NOT NULL DEFAULT 10000,
    container_tags text[],
    access_token   text,
    refresh_token  text,
    expires_at     timestamptz,
    metadata       jsonb NOT NULL DEFAULT '{}',
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS sync_runs (
    id              text PRIMARY KEY,
    connection_id   text NOT NULL,
    status          text NOT NULL DEFAULT 'running',
    trigger_type    text NOT NULL DEFAULT 'manual',
    error_kind      text,
    started_at      timestamptz NOT NULL DEFAULT now(),
    completed_at    timestamptz,
    items_processed int NOT NULL DEFAULT 0,
    items_failed    int NOT NULL DEFAULT 0,
    error           text
);
CREATE INDEX IF NOT EXISTS sync_runs_connection_idx ON sync_runs (connection_id);
