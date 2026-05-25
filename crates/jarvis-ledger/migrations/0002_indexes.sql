-- Migration 0002 — secondary indexes to keep query plans linear past ~10k events.
--
-- Notes on adapted names:
--   * `events(task_id)` is already covered by the composite `idx_events_task
--     (task_id, id)` from 0001 — SQLite uses it for plain `task_id = ?`
--     lookups too, so a standalone `idx_events_task_id` is redundant and
--     omitted.
--   * `events(kind, ts_micros)` from the spec maps to the actual `ts` column;
--     `idx_events_kind_ts` already exists in 0001. Re-creating it would be a
--     no-op so it is also omitted here.
--   * The remaining indexes target columns that were previously un-indexed
--     and that show up in hot query paths (parent-chain walks, memory scope
--     lookups, event reply chains).

-- Parent-chain walks on the events DAG (`parent_evt` is sparse — most events
-- have no parent — so a partial index keeps it small).
CREATE INDEX IF NOT EXISTS idx_events_parent_evt
    ON events(parent_evt) WHERE parent_evt IS NOT NULL;

-- Memory listings often filter by (scope, status) without a scope_value
-- (e.g. "all active global memories"); the existing composite
-- `(scope, scope_value, status)` from 0001 cannot satisfy that with an
-- index-only scan because scope_value sits in the middle.
CREATE INDEX IF NOT EXISTS idx_memories_scope_status
    ON memories(scope, status);

-- Task tree walks (`walk_ancestors`, recursive CTE in `timeline_events`)
-- read `tasks.parent` repeatedly.
CREATE INDEX IF NOT EXISTS idx_tasks_parent
    ON tasks(parent) WHERE parent IS NOT NULL;
