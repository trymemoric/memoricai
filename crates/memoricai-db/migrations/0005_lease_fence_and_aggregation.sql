-- H2: fence ingest job leases with a per-claim token so a stale worker cannot renew
-- another worker's lease or write to a document it no longer owns.
ALTER TABLE documents ADD COLUMN IF NOT EXISTS lease_token text;

-- M13: mark memories once they have been folded into a profile summary, so aggregation
-- does not re-summarize the same oldest 100 memories forever and can advance past them.
ALTER TABLE memories ADD COLUMN IF NOT EXISTS aggregated_at timestamptz;
