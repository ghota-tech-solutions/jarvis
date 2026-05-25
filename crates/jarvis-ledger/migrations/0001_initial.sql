-- Migration 0001 — initial ledger schema.
--
-- Extracted verbatim from the previous inline SCHEMA_SQL constant so existing
-- SQLite files (where every statement here has already been applied via
-- `CREATE … IF NOT EXISTS`) are upgraded transparently.

CREATE TABLE IF NOT EXISTS schema_migrations (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id BLOB PRIMARY KEY NOT NULL,
    parent BLOB REFERENCES tasks(id),
    goal TEXT NOT NULL,
    status TEXT NOT NULL,
    workdir TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    error TEXT,
    sandbox TEXT NOT NULL DEFAULT '',
    net_policy TEXT NOT NULL DEFAULT '',
    worktree_path TEXT NOT NULL DEFAULT '',
    worktree_branch TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status, created_at);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    task_id BLOB NOT NULL REFERENCES tasks(id),
    agent_id BLOB,
    kind TEXT NOT NULL,
    subject TEXT,
    payload TEXT NOT NULL,
    parent_evt INTEGER REFERENCES events(id)
);

CREATE INDEX IF NOT EXISTS idx_events_task ON events(task_id, id);
CREATE INDEX IF NOT EXISTS idx_events_subject ON events(subject) WHERE subject IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_kind_ts ON events(kind, ts);

-- Append-only enforcement.
CREATE TRIGGER IF NOT EXISTS events_no_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events table is append-only');
END;

CREATE TRIGGER IF NOT EXISTS events_no_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events table is append-only');
END;

-- M9: long-term memories. Unlike events, this table is mutable —
-- promote/forget/edit are first-class CRUD operations.
CREATE TABLE IF NOT EXISTS memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scope TEXT NOT NULL,                   -- 'workdir' | 'global'
    scope_value TEXT NOT NULL DEFAULT '',  -- workdir path; '' for global
    kind TEXT NOT NULL,                    -- 'pattern' | 'preference' | 'fact'
    text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'candidate', -- 'candidate' | 'active' | 'forgotten'
    source_task_id BLOB REFERENCES tasks(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    usage_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_memories_scope ON memories(scope, scope_value, status);
CREATE INDEX IF NOT EXISTS idx_memories_status ON memories(status, updated_at);

-- M12.S1: scheduled tasks. Cron string is validated server-side before
-- insert. last_task_id is the most recent enqueued child for UI display;
-- next_run_micros is cached for ordering ("upcoming runs" panel) but
-- recomputed each tick to survive cron-spec changes.
CREATE TABLE IF NOT EXISTS schedules (
    id TEXT PRIMARY KEY NOT NULL,        -- UUID v4 string
    cron TEXT NOT NULL,
    goal TEXT NOT NULL,
    workdir TEXT NOT NULL DEFAULT '',
    sandbox TEXT NOT NULL DEFAULT '',
    net_policy TEXT NOT NULL DEFAULT '',
    routing_policy TEXT NOT NULL DEFAULT '',
    max_steps INTEGER NOT NULL DEFAULT 0,
    label TEXT NOT NULL DEFAULT '',
    paused INTEGER NOT NULL DEFAULT 0,    -- 0=active, 1=paused
    last_run_micros INTEGER NOT NULL DEFAULT 0,
    next_run_micros INTEGER NOT NULL DEFAULT 0,
    last_task_id TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_schedules_active
    ON schedules(paused, next_run_micros);
