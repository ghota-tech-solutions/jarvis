use crate::model::{EventKind, EventRecord, NewEvent, TaskRecord, TaskRuntimeInfo, TaskStatus};
use chrono::Utc;
use jarvis_core::{AgentId, EventId, TaskId};
use serde_json::Value as Json;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found")]
    NotFound,
    #[error("invalid: {0}")]
    Invalid(String),
}

/// Cloneable ledger handle. Internally wraps a SqlitePool (so multiple readers
/// are fine) and a broadcast channel for live event streaming.
#[derive(Clone)]
pub struct Ledger {
    pool: SqlitePool,
    bus: Arc<broadcast::Sender<EventRecord>>,
}

impl Ledger {
    /// Open (or create) the ledger at `path`. Applies migrations.
    pub async fn open(path: &Path) -> Result<Self, LedgerError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            // Writes serialize at the SQLite layer; multiple connections still help reads.
            .connect_with(opts)
            .await?;

        Self::migrate(&pool).await?;

        let (tx, _) = broadcast::channel(1024);
        info!(path = %path.display(), "ledger open");
        Ok(Self {
            pool,
            bus: Arc::new(tx),
        })
    }

    async fn migrate(pool: &SqlitePool) -> Result<(), LedgerError> {
        // We embed our schema inline (no sqlx-migrate dir) to keep deployment simple.
        sqlx::query(SCHEMA_SQL).execute(pool).await?;
        Ok(())
    }

    /// Subscribe to live events. Receivers added AFTER an event was emitted miss it
    /// (use `query_events_since` to catch up).
    pub fn subscribe(&self) -> broadcast::Receiver<EventRecord> {
        self.bus.subscribe()
    }

    // ---------- Task ops ----------

    pub async fn create_task(
        &self,
        goal: &str,
        workdir: &str,
        parent: Option<TaskId>,
    ) -> Result<TaskRecord, LedgerError> {
        let id = TaskId::new();
        let now = now_micros();
        sqlx::query(
            "INSERT INTO tasks (id, parent, goal, status, workdir, created_at, \
             sandbox, net_policy, worktree_path, worktree_branch) \
             VALUES (?, ?, ?, ?, ?, ?, '', '', '', '')",
        )
        .bind(uuid_bytes(id.as_uuid()))
        .bind(parent.map(|p| uuid_bytes(p.as_uuid())))
        .bind(goal)
        .bind(TaskStatus::Pending.as_str())
        .bind(workdir)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(TaskRecord {
            id,
            parent,
            goal: goal.to_string(),
            status: TaskStatus::Pending,
            workdir: workdir.to_string(),
            created_at: now,
            completed_at: None,
            error: None,
            sandbox: String::new(),
            net_policy: String::new(),
            worktree_path: String::new(),
            worktree_branch: String::new(),
        })
    }

    /// Update the runtime metadata for a task (set once when the agent starts).
    pub async fn set_task_runtime(
        &self,
        id: TaskId,
        info: &TaskRuntimeInfo,
    ) -> Result<(), LedgerError> {
        sqlx::query(
            "UPDATE tasks SET sandbox = ?, net_policy = ?, worktree_path = ?, worktree_branch = ? \
             WHERE id = ?",
        )
        .bind(&info.sandbox)
        .bind(&info.net_policy)
        .bind(&info.worktree_path)
        .bind(&info.worktree_branch)
        .bind(uuid_bytes(id.as_uuid()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_task_status(
        &self,
        id: TaskId,
        status: TaskStatus,
        error: Option<&str>,
    ) -> Result<(), LedgerError> {
        let completed_at = status.is_finished().then(now_micros);
        sqlx::query(
            "UPDATE tasks SET status = ?, completed_at = COALESCE(?, completed_at), error = ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(completed_at)
        .bind(error)
        .bind(uuid_bytes(id.as_uuid()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_task(&self, id: TaskId) -> Result<TaskRecord, LedgerError> {
        let row = sqlx::query(TASK_SELECT_BY_ID)
            .bind(uuid_bytes(id.as_uuid()))
            .fetch_optional(&self.pool)
            .await?
            .ok_or(LedgerError::NotFound)?;
        row_to_task(&row)
    }

    pub async fn list_tasks(
        &self,
        include_finished: bool,
        limit: u32,
    ) -> Result<Vec<TaskRecord>, LedgerError> {
        let limit = if limit == 0 { 100 } else { limit as i64 };
        let rows = if include_finished {
            sqlx::query(TASK_SELECT_ALL).bind(limit).fetch_all(&self.pool).await?
        } else {
            sqlx::query(TASK_SELECT_ACTIVE).bind(limit).fetch_all(&self.pool).await?
        };
        rows.iter().map(row_to_task).collect()
    }

    // ---------- Event ops ----------

    /// Append an event. Assigns id and ts_micros. Broadcasts on the bus.
    pub async fn append(&self, ev: NewEvent) -> Result<EventRecord, LedgerError> {
        let ts = now_micros();
        let payload = serde_json::to_string(&ev.payload)?;
        let result = sqlx::query(
            "INSERT INTO events (ts, task_id, agent_id, kind, subject, payload, parent_evt) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ts)
        .bind(uuid_bytes(ev.task_id.as_uuid()))
        .bind(ev.agent_id.map(|a| uuid_bytes(a.as_uuid())))
        .bind(ev.kind.as_str())
        .bind(ev.subject.as_deref())
        .bind(&payload)
        .bind(ev.parent_evt.map(|e| e.0))
        .execute(&self.pool)
        .await?;

        let rec = EventRecord {
            id: EventId(result.last_insert_rowid()),
            ts_micros: ts,
            task_id: ev.task_id,
            agent_id: ev.agent_id,
            kind: ev.kind,
            subject: ev.subject,
            payload: ev.payload,
            parent_evt: ev.parent_evt,
        };
        let _ = self.bus.send(rec.clone());
        debug!(evt = %rec.id, task = %rec.task_id, kind = %rec.kind, "append");
        Ok(rec)
    }

    /// Stream historical events for a task (`task_id` empty = all), ordered by id ascending.
    pub async fn query_events(
        &self,
        task_id: Option<TaskId>,
        since_id: i64,
        limit: u32,
    ) -> Result<Vec<EventRecord>, LedgerError> {
        let limit = if limit == 0 { 10_000 } else { limit as i64 };
        let rows = if let Some(t) = task_id {
            sqlx::query(
                "SELECT id, ts, task_id, agent_id, kind, subject, payload, parent_evt \
                 FROM events WHERE task_id = ? AND id > ? ORDER BY id ASC LIMIT ?",
            )
            .bind(uuid_bytes(t.as_uuid()))
            .bind(since_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, ts, task_id, agent_id, kind, subject, payload, parent_evt \
                 FROM events WHERE id > ? ORDER BY id ASC LIMIT ?",
            )
            .bind(since_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };
        rows.iter().map(row_to_event).collect()
    }

    /// Walk the parent chain. Returns ancestors oldest→newest (root first),
    /// including the requested task itself at the end.
    pub async fn walk_ancestors(&self, leaf: TaskId) -> Result<Vec<TaskRecord>, LedgerError> {
        let mut chain: Vec<TaskRecord> = Vec::new();
        let mut current = Some(leaf);
        while let Some(id) = current {
            // Cycle guard.
            if chain.iter().any(|t| t.id == id) {
                break;
            }
            let t = self.get_task(id).await?;
            current = t.parent;
            chain.push(t);
            if chain.len() > 64 {
                // Hard cap; deeper chains are nearly always bugs.
                break;
            }
        }
        chain.reverse();
        Ok(chain)
    }

    /// Events for ANY of the given task ids, in id order.
    pub async fn query_events_multi(
        &self,
        task_ids: &[TaskId],
        since_id: i64,
        limit: u32,
    ) -> Result<Vec<EventRecord>, LedgerError> {
        if task_ids.is_empty() {
            return Ok(Vec::new());
        }
        let limit = if limit == 0 { 10_000 } else { limit as i64 };
        let placeholders = std::iter::repeat_n("?", task_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, ts, task_id, agent_id, kind, subject, payload, parent_evt \
             FROM events WHERE task_id IN ({placeholders}) AND id > ? ORDER BY id ASC LIMIT ?",
        );
        let mut q = sqlx::query(&sql);
        for t in task_ids {
            q = q.bind(uuid_bytes(t.as_uuid()));
        }
        q = q.bind(since_id).bind(limit);
        let rows = q.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_event).collect()
    }

    /// Every event for `root` and all its descendant tasks, ordered by id ASC.
    /// Walks the task-tree via the `tasks.parent` self-reference in a single
    /// recursive CTE — bounded by SQLite's default 1000-row recursion depth,
    /// which is far beyond any realistic agent fan-out.
    ///
    /// Used by the `GetTimeline` RPC to feed the scrubbable canvas timeline
    /// in the SolidJS SPA.
    pub async fn timeline_events(&self, root: TaskId) -> Result<Vec<EventRecord>, LedgerError> {
        let rows = sqlx::query(
            "WITH RECURSIVE descendants(id) AS ( \
                SELECT id FROM tasks WHERE id = ? \
                UNION ALL \
                SELECT t.id FROM tasks t JOIN descendants d ON t.parent = d.id \
             ) \
             SELECT e.id, e.ts, e.task_id, e.agent_id, e.kind, e.subject, e.payload, e.parent_evt \
             FROM events e JOIN descendants d ON e.task_id = d.id \
             ORDER BY e.id ASC",
        )
        .bind(uuid_bytes(root.as_uuid()))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_event).collect()
    }

    /// Fast last-N events for a task (used by agent loop to build context).
    pub async fn recent_events(
        &self,
        task_id: TaskId,
        n: u32,
    ) -> Result<Vec<EventRecord>, LedgerError> {
        let n = n.max(1) as i64;
        let mut rows = sqlx::query(
            "SELECT id, ts, task_id, agent_id, kind, subject, payload, parent_evt \
             FROM events WHERE task_id = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(uuid_bytes(task_id.as_uuid()))
        .bind(n)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_event)
        .collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Like `recent_events` but pre-filtered to the event kinds the agent's
    /// prompt builder actually consumes. Critical for context economy: a
    /// streamed LLM turn produces 20-30 `llm_chunk` rows plus a few
    /// heartbeat/attempt rows; a naive `recent_events(40)` therefore carries
    /// less than two real turns of history. This variant returns the last N
    /// `decision` / `tool_result` / `error` / `continuation` / `verdict`
    /// events so N maps to roughly N agent moves.
    // ---------- M9: memories ----------

    pub async fn create_memory(
        &self,
        m: crate::memory::NewMemory,
    ) -> Result<crate::memory::MemoryRecord, LedgerError> {
        let now = now_micros();
        let res = sqlx::query(
            "INSERT INTO memories \
             (scope, scope_value, kind, text, status, source_task_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(m.scope.as_str())
        .bind(&m.scope_value)
        .bind(m.kind.as_str())
        .bind(&m.text)
        .bind(m.status.as_str())
        .bind(m.source_task_id.map(|t| uuid_bytes(t.as_uuid())))
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(crate::memory::MemoryRecord {
            id: res.last_insert_rowid(),
            scope: m.scope,
            scope_value: m.scope_value,
            kind: m.kind,
            text: m.text,
            status: m.status,
            source_task_id: m.source_task_id,
            created_at: now,
            updated_at: now,
            usage_count: 0,
        })
    }

    pub async fn list_memories(
        &self,
        scope: Option<crate::memory::MemoryScope>,
        scope_value: Option<&str>,
        status: Option<crate::memory::MemoryStatus>,
        limit: u32,
    ) -> Result<Vec<crate::memory::MemoryRecord>, LedgerError> {
        let limit = if limit == 0 { 200 } else { limit as i64 };
        let mut sql = String::from(
            "SELECT id, scope, scope_value, kind, text, status, source_task_id, \
                    created_at, updated_at, usage_count \
             FROM memories WHERE 1=1",
        );
        if scope.is_some() {
            sql.push_str(" AND scope = ?");
        }
        if scope_value.is_some() {
            sql.push_str(" AND scope_value = ?");
        }
        if status.is_some() {
            sql.push_str(" AND status = ?");
        } else {
            sql.push_str(" AND status != 'forgotten'");
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT ?");
        let mut q = sqlx::query(&sql);
        if let Some(s) = scope {
            q = q.bind(s.as_str());
        }
        if let Some(v) = scope_value {
            q = q.bind(v.to_string());
        }
        if let Some(s) = status {
            q = q.bind(s.as_str());
        }
        q = q.bind(limit);
        let rows = q.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_memory).collect()
    }

    pub async fn get_memory(
        &self,
        id: i64,
    ) -> Result<crate::memory::MemoryRecord, LedgerError> {
        let row = sqlx::query(
            "SELECT id, scope, scope_value, kind, text, status, source_task_id, \
                    created_at, updated_at, usage_count \
             FROM memories WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(LedgerError::NotFound)?;
        row_to_memory(&row)
    }

    pub async fn set_memory_status(
        &self,
        id: i64,
        status: crate::memory::MemoryStatus,
        new_text: Option<&str>,
    ) -> Result<crate::memory::MemoryRecord, LedgerError> {
        let now = now_micros();
        if let Some(t) = new_text {
            sqlx::query(
                "UPDATE memories SET status = ?, text = ?, updated_at = ? WHERE id = ?",
            )
            .bind(status.as_str())
            .bind(t)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query("UPDATE memories SET status = ?, updated_at = ? WHERE id = ?")
                .bind(status.as_str())
                .bind(now)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        self.get_memory(id).await
    }

    pub async fn edit_memory(
        &self,
        id: i64,
        text: &str,
        scope: crate::memory::MemoryScope,
        scope_value: &str,
        kind: crate::memory::MemoryKind,
    ) -> Result<crate::memory::MemoryRecord, LedgerError> {
        let now = now_micros();
        sqlx::query(
            "UPDATE memories SET text = ?, scope = ?, scope_value = ?, kind = ?, \
                    updated_at = ? WHERE id = ?",
        )
        .bind(text)
        .bind(scope.as_str())
        .bind(scope_value)
        .bind(kind.as_str())
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.get_memory(id).await
    }

    /// Return all `active` memories whose scope matches the given workdir,
    /// plus all `active` `global` memories. Used by the agent loop to inject
    /// learnings into the system prompt.
    pub async fn active_memories_for_workdir(
        &self,
        workdir: &str,
    ) -> Result<Vec<crate::memory::MemoryRecord>, LedgerError> {
        let rows = sqlx::query(
            "SELECT id, scope, scope_value, kind, text, status, source_task_id, \
                    created_at, updated_at, usage_count \
             FROM memories \
             WHERE status = 'active' AND (scope = 'global' OR (scope = 'workdir' AND scope_value = ?)) \
             ORDER BY updated_at DESC LIMIT 64",
        )
        .bind(workdir)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_memory).collect()
    }

    pub async fn increment_memory_usage(&self, id: i64) -> Result<(), LedgerError> {
        sqlx::query("UPDATE memories SET usage_count = usage_count + 1 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn recent_relevant_events(
        &self,
        task_id: TaskId,
        n: u32,
    ) -> Result<Vec<EventRecord>, LedgerError> {
        let n = n.max(1) as i64;
        let mut rows = sqlx::query(
            "SELECT id, ts, task_id, agent_id, kind, subject, payload, parent_evt \
             FROM events WHERE task_id = ? \
             AND kind IN ('decision','tool_result','error','continuation','verdict') \
             ORDER BY id DESC LIMIT ?",
        )
        .bind(uuid_bytes(task_id.as_uuid()))
        .bind(n)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_to_event)
        .collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }
}

fn row_to_task(row: &sqlx::sqlite::SqliteRow) -> Result<TaskRecord, LedgerError> {
    let id_bytes: Vec<u8> = row.try_get("id")?;
    let parent_bytes: Option<Vec<u8>> = row.try_get("parent")?;
    let goal: String = row.try_get("goal")?;
    let status_str: String = row.try_get("status")?;
    let workdir: String = row.try_get("workdir")?;
    let created_at: i64 = row.try_get("created_at")?;
    let completed_at: Option<i64> = row.try_get("completed_at")?;
    let error: Option<String> = row.try_get("error")?;
    let sandbox: String = row.try_get("sandbox").unwrap_or_default();
    let net_policy: String = row.try_get("net_policy").unwrap_or_default();
    let worktree_path: String = row.try_get("worktree_path").unwrap_or_default();
    let worktree_branch: String = row.try_get("worktree_branch").unwrap_or_default();
    Ok(TaskRecord {
        id: TaskId(uuid_from_bytes(&id_bytes)?),
        parent: parent_bytes
            .map(|b| uuid_from_bytes(&b).map(TaskId))
            .transpose()?,
        goal,
        status: TaskStatus::from_str(&status_str)
            .map_err(|_| LedgerError::Invalid(format!("status: {status_str}")))?,
        workdir,
        created_at,
        completed_at,
        error,
        sandbox,
        net_policy,
        worktree_path,
        worktree_branch,
    })
}

const TASK_COLUMNS: &str =
    "id, parent, goal, status, workdir, created_at, completed_at, error, sandbox, net_policy, worktree_path, worktree_branch";

const TASK_SELECT_BY_ID: &str =
    "SELECT id, parent, goal, status, workdir, created_at, completed_at, error, \
            sandbox, net_policy, worktree_path, worktree_branch \
     FROM tasks WHERE id = ?";

const TASK_SELECT_ALL: &str =
    "SELECT id, parent, goal, status, workdir, created_at, completed_at, error, \
            sandbox, net_policy, worktree_path, worktree_branch \
     FROM tasks ORDER BY created_at DESC LIMIT ?";

const TASK_SELECT_ACTIVE: &str =
    "SELECT id, parent, goal, status, workdir, created_at, completed_at, error, \
            sandbox, net_policy, worktree_path, worktree_branch \
     FROM tasks WHERE status IN ('pending','running') ORDER BY created_at DESC LIMIT ?";

#[allow(dead_code)]
const _TASK_COLUMNS_KEEP: &str = TASK_COLUMNS;

fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> Result<EventRecord, LedgerError> {
    let id: i64 = row.try_get("id")?;
    let ts: i64 = row.try_get("ts")?;
    let task_bytes: Vec<u8> = row.try_get("task_id")?;
    let agent_bytes: Option<Vec<u8>> = row.try_get("agent_id")?;
    let kind_str: String = row.try_get("kind")?;
    let subject: Option<String> = row.try_get("subject")?;
    let payload_str: String = row.try_get("payload")?;
    let parent_evt: Option<i64> = row.try_get("parent_evt")?;
    let payload: Json = serde_json::from_str(&payload_str).unwrap_or(Json::Null);
    Ok(EventRecord {
        id: EventId(id),
        ts_micros: ts,
        task_id: TaskId(uuid_from_bytes(&task_bytes)?),
        agent_id: agent_bytes
            .map(|b| uuid_from_bytes(&b).map(AgentId))
            .transpose()?,
        kind: EventKind::from_str(&kind_str)
            .map_err(|_| LedgerError::Invalid(format!("kind: {kind_str}")))?,
        subject,
        payload,
        parent_evt: parent_evt.map(EventId),
    })
}

fn row_to_memory(row: &sqlx::sqlite::SqliteRow) -> Result<crate::memory::MemoryRecord, LedgerError> {
    use std::str::FromStr;
    let id: i64 = row.try_get("id")?;
    let scope_s: String = row.try_get("scope")?;
    let scope_value: String = row.try_get("scope_value")?;
    let kind_s: String = row.try_get("kind")?;
    let text: String = row.try_get("text")?;
    let status_s: String = row.try_get("status")?;
    let source_bytes: Option<Vec<u8>> = row.try_get("source_task_id")?;
    let created_at: i64 = row.try_get("created_at")?;
    let updated_at: i64 = row.try_get("updated_at")?;
    let usage_count: i64 = row.try_get("usage_count")?;
    Ok(crate::memory::MemoryRecord {
        id,
        scope: crate::memory::MemoryScope::from_str(&scope_s)
            .map_err(|_| LedgerError::Invalid(format!("memory scope: {scope_s}")))?,
        scope_value,
        kind: crate::memory::MemoryKind::from_str(&kind_s)
            .map_err(|_| LedgerError::Invalid(format!("memory kind: {kind_s}")))?,
        text,
        status: crate::memory::MemoryStatus::from_str(&status_s)
            .map_err(|_| LedgerError::Invalid(format!("memory status: {status_s}")))?,
        source_task_id: source_bytes
            .map(|b| uuid_from_bytes(&b).map(jarvis_core::TaskId))
            .transpose()?,
        created_at,
        updated_at,
        usage_count,
    })
}

fn uuid_bytes(u: &Uuid) -> Vec<u8> {
    u.as_bytes().to_vec()
}

fn uuid_from_bytes(b: &[u8]) -> Result<Uuid, LedgerError> {
    Uuid::from_slice(b).map_err(|e| LedgerError::Invalid(format!("uuid: {e}")))
}

fn now_micros() -> i64 {
    Utc::now().timestamp_micros()
}

/// Embedded schema. Single-statement-per-`execute` is fine because sqlx
/// supports multi-statement query strings on SQLite.
const SCHEMA_SQL: &str = r#"
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
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EventKind;
    use serde_json::json;

    async fn open_temp() -> Ledger {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        // tempdir() outlives the await thanks to manual leak; in real tests we'd hold it.
        std::mem::forget(dir);
        Ledger::open(&path).await.unwrap()
    }

    #[tokio::test]
    async fn create_task_and_append_event() {
        let l = open_temp().await;
        let t = l.create_task("test goal", ".", None).await.unwrap();
        let ev = l
            .append(NewEvent::new(t.id, EventKind::Decision, json!({"thought": "hi"})))
            .await
            .unwrap();
        assert_eq!(ev.task_id, t.id);
        assert!(ev.id.0 >= 1);

        let recent = l.recent_events(t.id, 10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].kind, EventKind::Decision);
    }

    #[tokio::test]
    async fn events_are_append_only() {
        let l = open_temp().await;
        let t = l.create_task("g", ".", None).await.unwrap();
        l.append(NewEvent::new(t.id, EventKind::Decision, json!({})))
            .await
            .unwrap();
        // Try to mutate — should be rejected by trigger.
        let err = sqlx::query("UPDATE events SET kind = 'tampered' WHERE task_id = ?")
            .bind(uuid_bytes(t.id.as_uuid()))
            .execute(&l.pool)
            .await
            .err();
        assert!(err.is_some(), "expected trigger to reject UPDATE");
    }

    #[tokio::test]
    async fn live_broadcast() {
        let l = open_temp().await;
        let t = l.create_task("g", ".", None).await.unwrap();
        let mut rx = l.subscribe();
        let ev = l
            .append(NewEvent::new(t.id, EventKind::ToolCall, json!({"tool":"shell"})))
            .await
            .unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("recv timeout")
            .expect("recv error");
        assert_eq!(received.id, ev.id);
    }

    #[tokio::test]
    async fn set_status_to_completed_marks_completed_at() {
        let l = open_temp().await;
        let t = l.create_task("g", ".", None).await.unwrap();
        l.set_task_status(t.id, TaskStatus::Completed, None).await.unwrap();
        let after = l.get_task(t.id).await.unwrap();
        assert_eq!(after.status, TaskStatus::Completed);
        assert!(after.completed_at.is_some());
    }

    #[tokio::test]
    async fn timeline_events_walks_descendants_recursively() {
        let l = open_temp().await;
        let root = l.create_task("root", ".", None).await.unwrap();
        let child = l.create_task("child", ".", Some(root.id)).await.unwrap();
        let grandchild = l.create_task("grandchild", ".", Some(child.id)).await.unwrap();
        let sibling_orphan = l.create_task("orphan", ".", None).await.unwrap();

        // Mix of events across the three connected tasks + the orphan.
        l.append(NewEvent::new(root.id, EventKind::Decision, json!({"r": 1}))).await.unwrap();
        l.append(NewEvent::new(child.id, EventKind::ToolCall, json!({"tool": "shell"}))).await.unwrap();
        l.append(NewEvent::new(grandchild.id, EventKind::Verdict, json!({"v": "pass"}))).await.unwrap();
        l.append(NewEvent::new(sibling_orphan.id, EventKind::Decision, json!({"o": 1}))).await.unwrap();

        let timeline = l.timeline_events(root.id).await.unwrap();
        assert_eq!(timeline.len(), 3, "orphan task's event must not appear");
        assert!(timeline.windows(2).all(|w| w[0].id.0 < w[1].id.0), "must be id-ascending");
        let kinds: Vec<EventKind> = timeline.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![EventKind::Decision, EventKind::ToolCall, EventKind::Verdict]);
    }

    #[tokio::test]
    async fn timeline_events_empty_for_unknown_root() {
        let l = open_temp().await;
        let phantom = TaskId(Uuid::new_v4());
        let timeline = l.timeline_events(phantom).await.unwrap();
        assert!(timeline.is_empty());
    }
}

