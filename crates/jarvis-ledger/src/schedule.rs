//! Scheduled task definitions — cron-string + agent-loop dispatch metadata.
//!
//! The ledger only stores the spec + run-tracking columns; the actual cron
//! parsing + fire loop lives in `jarvis-daemon`. This crate is leafward so
//! it must not depend on tokio-cron-scheduler.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRecord {
    pub id: String,
    pub cron: String,
    pub goal: String,
    pub workdir: String,
    pub sandbox: String,
    pub net_policy: String,
    pub routing_policy: String,
    pub max_steps: u32,
    pub label: String,
    pub paused: bool,
    pub last_run_micros: i64,
    pub next_run_micros: i64,
    pub last_task_id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub id: String,
    pub cron: String,
    pub goal: String,
    pub workdir: String,
    pub sandbox: String,
    pub net_policy: String,
    pub routing_policy: String,
    pub max_steps: u32,
    pub label: String,
    pub paused: bool,
}
