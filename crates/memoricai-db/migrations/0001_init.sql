-- Complete initial schema for a fresh memoricai installation.

CREATE EXTENSION IF NOT EXISTS vector;

-- ---------------- identity ----------------

CREATE TABLE users (
    id    text PRIMARY KEY,
    email text NOT NULL UNIQUE,
    name  text
);

CREATE TABLE organizations (
    id       text PRIMARY KEY,
    name     text NOT NULL,
    metadata jsonb NOT NULL DEFAULT '{}'
);

CREATE TABLE members (
    user_id        text NOT NULL,
    org_id         text NOT NULL,
    role           text NOT NULL DEFAULT 'member',
    access_type    text NOT NULL DEFAULT 'full',
    container_tags text[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (user_id, org_id)
);

CREATE TABLE api_keys (
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
CREATE INDEX api_keys_prefix_idx ON api_keys (prefix);
CREATE INDEX api_keys_prefix_last4_idx ON api_keys (prefix, last4) WHERE NOT revoked;

-- ---------------- spaces / projects ----------------

CREATE TABLE spaces (
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

-- ---------------- connectors ----------------

CREATE TABLE connections (
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
    sync_cursor    text,
    last_synced_at timestamptz,
    metadata       jsonb NOT NULL DEFAULT '{}',
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sync_runs (
    id              text PRIMARY KEY,
    connection_id   text NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    status          text NOT NULL DEFAULT 'running',
    trigger_type    text NOT NULL DEFAULT 'manual',
    error_kind      text,
    started_at      timestamptz NOT NULL DEFAULT now(),
    completed_at    timestamptz,
    lease_until     timestamptz,
    items_processed int NOT NULL DEFAULT 0,
    items_failed    int NOT NULL DEFAULT 0,
    error           text
);
CREATE INDEX sync_runs_connection_idx ON sync_runs (connection_id);
CREATE UNIQUE INDEX sync_runs_one_running_idx
    ON sync_runs (connection_id) WHERE status='running';
CREATE INDEX connections_due_idx
    ON connections (last_synced_at NULLS FIRST, id);

-- ---------------- documents ----------------

CREATE TABLE documents (
    id                  text PRIMARY KEY,
    custom_id           text,
    content_hash        text,
    org_id              text NOT NULL,
    user_id             text,
    connection_id       text REFERENCES connections(id) ON DELETE SET NULL,
    title               text,
    summary             text,
    content             text,
    raw                 text,
    url                 text,
    source              text,
    doc_type            text NOT NULL DEFAULT 'text',
    status              text NOT NULL DEFAULT 'queued',
    metadata            jsonb NOT NULL DEFAULT '{}',
    container_tags      text[] NOT NULL DEFAULT '{}',
    token_count         bigint,
    chunk_count         bigint DEFAULT 0,
    processing_attempts int NOT NULL DEFAULT 0,
    lease_until         timestamptz,
    lease_token         text,
    last_error          text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX documents_org_custom_id_idx
    ON documents (org_id, custom_id) WHERE custom_id IS NOT NULL;
CREATE INDEX documents_org_idx ON documents (org_id);
CREATE INDEX documents_container_tags_idx ON documents USING gin (container_tags);
CREATE INDEX documents_status_idx ON documents (status);
CREATE INDEX documents_ingest_queue_idx ON documents (status, lease_until, updated_at);
CREATE INDEX documents_org_created_idx
    ON documents (org_id, created_at DESC, id);
CREATE INDEX documents_org_status_created_idx
    ON documents (org_id, status, created_at DESC, id);
CREATE INDEX documents_org_connection_idx
    ON documents (org_id, connection_id)
    WHERE connection_id IS NOT NULL;

-- ---------------- memories ----------------

CREATE TABLE memories (
    id                  text PRIMARY KEY,
    custom_id           text,
    document_id         text REFERENCES documents(id) ON DELETE SET NULL,
    org_id              text NOT NULL,
    user_id             text,
    memory              text NOT NULL,
    summary             text,
    mem_type            text,
    space_container_tag text NOT NULL,
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
    event_date          timestamptz,
    bucket_key          text,
    aggregated_at       timestamptz,
    metadata            jsonb NOT NULL DEFAULT '{}',
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX memories_scope_idx ON memories (org_id, space_container_tag);
CREATE INDEX memories_document_idx ON memories (document_id);
CREATE INDEX memories_root_idx ON memories (root_memory_id);
CREATE UNIQUE INDEX memories_latest_per_root_idx
    ON memories (root_memory_id) WHERE is_latest AND root_memory_id IS NOT NULL;
CREATE INDEX memories_bucket_idx ON memories (org_id, space_container_tag, bucket_key);
CREATE INDEX idx_memories_event_date
    ON memories (space_container_tag, event_date) WHERE event_date IS NOT NULL;
CREATE INDEX memories_profile_static_idx
    ON memories (org_id, space_container_tag, updated_at DESC, id)
    WHERE is_latest AND NOT is_forgotten AND is_static;
CREATE INDEX memories_profile_dynamic_idx
    ON memories (org_id, space_container_tag, created_at DESC, id)
    WHERE is_latest AND NOT is_forgotten AND NOT is_static;
CREATE INDEX memories_bucket_created_idx
    ON memories (org_id, space_container_tag, bucket_key, created_at DESC, id)
    WHERE is_latest AND NOT is_forgotten AND bucket_key IS NOT NULL;
CREATE INDEX memories_forget_after_idx
    ON memories (forget_after)
    WHERE forget_after IS NOT NULL AND NOT is_forgotten;

CREATE TABLE memory_relations (
    source_memory_id text NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    target_memory_id text NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    relation         text NOT NULL,
    PRIMARY KEY (source_memory_id, target_memory_id)
);
CREATE INDEX memory_relations_target_idx ON memory_relations (target_memory_id);

-- ---------------- chunks ----------------

CREATE TABLE chunks (
    id          text PRIMARY KEY,
    document_id text NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    org_id      text NOT NULL,
    content     text NOT NULL,
    chunk_type  text NOT NULL DEFAULT 'text',
    position    int NOT NULL DEFAULT 0,
    metadata    jsonb NOT NULL DEFAULT '{}',
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX chunks_document_idx ON chunks (document_id);

CREATE TABLE chunk_containers (
    chunk_id      text NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    container_tag text NOT NULL,
    PRIMARY KEY (chunk_id, container_tag)
);
CREATE INDEX chunk_containers_tag_chunk_idx
    ON chunk_containers (container_tag, chunk_id);

-- ---------------- versioned embeddings ----------------

CREATE TABLE embedding_indexes (
    id                 text PRIMARY KEY,
    org_id             text NOT NULL,
    embedding_model_id text NOT NULL,
    model_version      text NOT NULL,
    provider           text NOT NULL,
    dimension          int NOT NULL CHECK (dimension > 0),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, provider, embedding_model_id, model_version, dimension)
);
CREATE INDEX embedding_indexes_org_idx ON embedding_indexes (org_id, created_at);

CREATE TABLE memory_embeddings (
    index_id   text NOT NULL REFERENCES embedding_indexes(id) ON DELETE CASCADE,
    memory_id  text NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    embedding  vector NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (index_id, memory_id)
);
CREATE INDEX memory_embeddings_memory_idx ON memory_embeddings (memory_id);

CREATE TABLE chunk_embeddings (
    index_id   text NOT NULL REFERENCES embedding_indexes(id) ON DELETE CASCADE,
    chunk_id   text NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    embedding  vector NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (index_id, chunk_id)
);
CREATE INDEX chunk_embeddings_chunk_idx ON chunk_embeddings (chunk_id);

CREATE TABLE embedding_backfill_jobs (
    index_id           text PRIMARY KEY REFERENCES embedding_indexes(id) ON DELETE CASCADE,
    status             text NOT NULL DEFAULT 'queued'
                       CHECK (status IN ('queued', 'running', 'done', 'failed')),
    processed_memories bigint NOT NULL DEFAULT 0,
    processed_chunks   bigint NOT NULL DEFAULT 0,
    failure_count      int NOT NULL DEFAULT 0,
    lease_token        text,
    lease_until        timestamptz,
    next_attempt_at    timestamptz NOT NULL DEFAULT now(),
    last_error         text,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    completed_at       timestamptz
);
CREATE INDEX embedding_backfill_jobs_claim_idx
    ON embedding_backfill_jobs (status, next_attempt_at, lease_until);

-- ---------------- organization settings / profiles ----------------

CREATE TABLE organization_settings (
    org_id            text PRIMARY KEY,
    should_llm_filter boolean NOT NULL DEFAULT false,
    filter_prompt     text,
    categories        text[],
    include_items     text[],
    exclude_items     text[],
    chunk_size        int NOT NULL DEFAULT -1,
    updated_at        timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE profile_buckets (
    id            text PRIMARY KEY,
    org_id        text NOT NULL,
    container_tag text,
    key           text NOT NULL,
    description   text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX profile_buckets_unique_idx
    ON profile_buckets (org_id, coalesce(container_tag, ''), key);

CREATE TABLE profile_summaries (
    id            text PRIMARY KEY,
    org_id        text NOT NULL,
    container_tag text NOT NULL,
    bucket_key    text,
    summary       text NOT NULL,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX profile_summaries_unique_idx
    ON profile_summaries (org_id, container_tag, coalesce(bucket_key, ''));

-- ---------------- analytics ----------------

CREATE TABLE api_requests (
    id          text PRIMARY KEY,
    type        text NOT NULL,
    org_id      text,
    user_id     text,
    key_id      text,
    status_code int,
    duration    bigint,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX api_requests_org_idx ON api_requests (org_id, created_at);
CREATE INDEX api_requests_created_at_idx ON api_requests (created_at);

-- ---------------- OAuth2 / OIDC ----------------

CREATE TABLE oauth_clients (
    id            text PRIMARY KEY,
    client_secret text,
    name          text NOT NULL,
    redirect_uris text[] NOT NULL DEFAULT '{}',
    grant_types   text[] NOT NULL DEFAULT '{authorization_code,refresh_token}',
    first_party   boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oauth_codes (
    code                  text PRIMARY KEY,
    client_id             text NOT NULL,
    user_id               text NOT NULL,
    org_id                text NOT NULL,
    redirect_uri          text NOT NULL,
    code_challenge        text,
    code_challenge_method text,
    scope                 text,
    container_tags        text[] NOT NULL DEFAULT '{}',
    permission            text NOT NULL DEFAULT 'write',
    expires_at            timestamptz NOT NULL,
    created_at            timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oauth_tokens (
    access_token       text PRIMARY KEY,
    refresh_token      text,
    client_id          text NOT NULL,
    user_id            text NOT NULL,
    org_id             text NOT NULL,
    container_tags     text[] NOT NULL DEFAULT '{}',
    scope              text,
    permission         text NOT NULL DEFAULT 'write',
    access_expires_at  timestamptz NOT NULL,
    refresh_expires_at timestamptz,
    revoked            boolean NOT NULL DEFAULT false,
    created_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX oauth_tokens_refresh_idx ON oauth_tokens (refresh_token);

CREATE TABLE connection_state (
    state_token    text PRIMARY KEY,
    provider       text NOT NULL,
    org_id         text NOT NULL,
    user_id        text,
    redirect_url   text,
    container_tags text[] NOT NULL DEFAULT '{}',
    document_limit int NOT NULL DEFAULT 10000,
    metadata       jsonb NOT NULL DEFAULT '{}',
    expires_at     timestamptz NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
