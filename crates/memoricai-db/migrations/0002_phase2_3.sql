-- Phase 2 + 3 schema: profile buckets/summaries, OAuth2 provider, connector state.

-- ---------------- profile buckets & summaries ----------------

CREATE TABLE IF NOT EXISTS profile_buckets (
    id            text PRIMARY KEY,
    org_id        text NOT NULL,
    container_tag text,                 -- NULL = org-level bucket
    key           text NOT NULL,
    description   text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS profile_buckets_unique_idx
    ON profile_buckets (org_id, coalesce(container_tag, ''), key);

CREATE TABLE IF NOT EXISTS profile_summaries (
    id            text PRIMARY KEY,
    org_id        text NOT NULL,
    container_tag text NOT NULL,
    bucket_key    text,                 -- NULL = general dynamic summary
    summary       text NOT NULL,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS profile_summaries_unique_idx
    ON profile_summaries (org_id, container_tag, coalesce(bucket_key, ''));

-- Bucket assignment on memories (classifier output).
ALTER TABLE memories ADD COLUMN IF NOT EXISTS bucket_key text;
CREATE INDEX IF NOT EXISTS memories_bucket_idx ON memories (org_id, space_container_tag, bucket_key);

-- ---------------- OAuth2 / OIDC provider ----------------

CREATE TABLE IF NOT EXISTS oauth_clients (
    id            text PRIMARY KEY,       -- client_id
    client_secret text,                   -- NULL for public/PKCE clients
    name          text NOT NULL,
    redirect_uris text[] NOT NULL DEFAULT '{}',
    grant_types   text[] NOT NULL DEFAULT '{authorization_code,refresh_token}',
    first_party   boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS oauth_codes (
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

CREATE TABLE IF NOT EXISTS oauth_tokens (
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
CREATE INDEX IF NOT EXISTS oauth_tokens_refresh_idx ON oauth_tokens (refresh_token);

-- ---------------- connector OAuth CSRF state ----------------

CREATE TABLE IF NOT EXISTS connection_state (
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

-- Connector cursor/state for incremental sync + last sync time.
ALTER TABLE connections ADD COLUMN IF NOT EXISTS sync_cursor text;
ALTER TABLE connections ADD COLUMN IF NOT EXISTS last_synced_at timestamptz;
