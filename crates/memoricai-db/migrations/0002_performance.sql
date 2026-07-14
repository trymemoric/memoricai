-- Purpose-built indexes for the API, connector, profile, and retention hot paths.
-- Vector HNSW indexes are created by Db::ensure_ann_indexes because their typmod
-- depends on each embedding index's runtime-configured dimension and concurrent
-- index creation cannot run inside the migration transaction.

CREATE INDEX IF NOT EXISTS documents_org_created_idx
    ON documents (org_id, created_at DESC, id);
CREATE INDEX IF NOT EXISTS documents_org_status_created_idx
    ON documents (org_id, status, created_at DESC, id);
CREATE INDEX IF NOT EXISTS documents_org_connection_idx
    ON documents (org_id, connection_id)
    WHERE connection_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS connections_due_idx
    ON connections (last_synced_at NULLS FIRST, id);

CREATE INDEX IF NOT EXISTS memories_profile_static_idx
    ON memories (org_id, space_container_tag, updated_at DESC, id)
    WHERE is_latest AND NOT is_forgotten AND is_static;
CREATE INDEX IF NOT EXISTS memories_profile_dynamic_idx
    ON memories (org_id, space_container_tag, created_at DESC, id)
    WHERE is_latest AND NOT is_forgotten AND NOT is_static;
CREATE INDEX IF NOT EXISTS memories_bucket_created_idx
    ON memories (org_id, space_container_tag, bucket_key, created_at DESC, id)
    WHERE is_latest AND NOT is_forgotten AND bucket_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS memories_forget_after_idx
    ON memories (forget_after)
    WHERE forget_after IS NOT NULL AND NOT is_forgotten;
