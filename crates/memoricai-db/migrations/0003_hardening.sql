-- Security/reliability hardening: durable ingest leases and referential cleanup.

ALTER TABLE documents ADD COLUMN IF NOT EXISTS processing_attempts int NOT NULL DEFAULT 0;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS lease_until timestamptz;
ALTER TABLE documents ADD COLUMN IF NOT EXISTS last_error text;
ALTER TABLE sync_runs ADD COLUMN IF NOT EXISTS lease_until timestamptz;

CREATE INDEX IF NOT EXISTS documents_ingest_queue_idx
    ON documents (status, lease_until, updated_at);

CREATE INDEX IF NOT EXISTS api_keys_prefix_last4_idx
    ON api_keys (prefix, last4) WHERE NOT revoked;

UPDATE sync_runs SET status='failed', completed_at=now(), error_kind='abandoned',
                     error='server stopped before sync completed', lease_until=NULL
WHERE status='running';

CREATE UNIQUE INDEX IF NOT EXISTS sync_runs_one_running_idx
    ON sync_runs (connection_id) WHERE status='running';

-- Existing deployments may contain rows orphaned by pre-hardening bulk deletes.
DELETE FROM memory_relations relation
WHERE NOT EXISTS (SELECT 1 FROM memories memory WHERE memory.id = relation.source_memory_id)
   OR NOT EXISTS (SELECT 1 FROM memories memory WHERE memory.id = relation.target_memory_id);

DELETE FROM chunks chunk
WHERE NOT EXISTS (SELECT 1 FROM documents document WHERE document.id = chunk.document_id);

DELETE FROM memories memory
WHERE memory.document_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM documents document WHERE document.id = memory.document_id);

DELETE FROM memory_relations relation
WHERE NOT EXISTS (SELECT 1 FROM memories memory WHERE memory.id = relation.source_memory_id)
   OR NOT EXISTS (SELECT 1 FROM memories memory WHERE memory.id = relation.target_memory_id);

DELETE FROM sync_runs run
WHERE NOT EXISTS (SELECT 1 FROM connections connection WHERE connection.id = run.connection_id);

UPDATE documents document SET connection_id = NULL
WHERE connection_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM connections connection WHERE connection.id = document.connection_id);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'chunks_document_fk') THEN
        ALTER TABLE chunks ADD CONSTRAINT chunks_document_fk
            FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE CASCADE;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'memories_document_fk') THEN
        ALTER TABLE memories ADD CONSTRAINT memories_document_fk
            FOREIGN KEY (document_id) REFERENCES documents(id) ON DELETE SET NULL;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'memory_relations_source_fk') THEN
        ALTER TABLE memory_relations ADD CONSTRAINT memory_relations_source_fk
            FOREIGN KEY (source_memory_id) REFERENCES memories(id) ON DELETE CASCADE;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'memory_relations_target_fk') THEN
        ALTER TABLE memory_relations ADD CONSTRAINT memory_relations_target_fk
            FOREIGN KEY (target_memory_id) REFERENCES memories(id) ON DELETE CASCADE;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'sync_runs_connection_fk') THEN
        ALTER TABLE sync_runs ADD CONSTRAINT sync_runs_connection_fk
            FOREIGN KEY (connection_id) REFERENCES connections(id) ON DELETE CASCADE;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'documents_connection_fk') THEN
        ALTER TABLE documents ADD CONSTRAINT documents_connection_fk
            FOREIGN KEY (connection_id) REFERENCES connections(id) ON DELETE SET NULL;
    END IF;
END $$;
