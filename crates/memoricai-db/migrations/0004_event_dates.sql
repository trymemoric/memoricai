-- Typed event dates: when the fact's underlying event happened (distinct from
-- the session/document date and from forget_after). Populated by extraction;
-- nullable for facts that are not tied to a specific date.
ALTER TABLE memories ADD COLUMN IF NOT EXISTS event_date TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_memories_event_date
    ON memories (space_container_tag, event_date)
    WHERE event_date IS NOT NULL;
