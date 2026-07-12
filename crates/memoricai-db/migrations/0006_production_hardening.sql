-- Production hardening: bound legacy unlimited owner keys and make analytics
-- retention cleanup efficient.

UPDATE api_keys
SET rate_limit_max = 5000, rate_limit_window_ms = 60000
WHERE key_type = 'org' AND rate_limit_max <= 0;

CREATE INDEX IF NOT EXISTS api_requests_created_at_idx
    ON api_requests (created_at);
