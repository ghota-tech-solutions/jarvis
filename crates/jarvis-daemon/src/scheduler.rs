//! Scheduled tasks — wakes the daemon's `submit_task` on a cron schedule.
//!
//! Each active schedule owns one tokio task that sleeps until its next
//! cron firing, calls `submit_task` internally, updates the ledger's
//! `last_run_micros` + `last_task_id` + `next_run_micros`, and loops.
//!
//! Booting: at daemon startup we load every non-paused schedule from the
//! ledger and spawn one task per row. `CreateSchedule` / `DeleteSchedule`
//! manage the live map after that. `RunScheduleNow` fires a submission
//! immediately and returns the new task handle without touching the loop.

use anyhow::{Result, anyhow};
use chrono::{TimeZone, Utc};
use cron::Schedule as CronSchedule;
use jarvis_ledger::{Ledger, NewSchedule, ScheduleRecord};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub type SchedulerHandles = Arc<Mutex<HashMap<String, CancellationToken>>>;

/// Validate a cron expression. Accepts both the classic 5-field unix cron
/// (`min hour dom mon dow`) and the 6-field with-seconds form (`sec min
/// hour dom mon dow`) by auto-prepending `0` (top of minute) for the
/// 5-field case before handing it to the parser.
pub fn validate_cron(expr: &str) -> Result<()> {
    CronSchedule::from_str(&normalize_cron(expr))
        .map(|_| ())
        .map_err(|e| anyhow!("invalid cron expression `{expr}`: {e}"))
}

/// Compute the next firing of a cron expression, in unix micros. Returns 0
/// when the schedule has no future occurrence (impossible per cron grammar
/// but defensive).
pub fn next_run_micros(expr: &str) -> i64 {
    let Ok(schedule) = CronSchedule::from_str(&normalize_cron(expr)) else {
        return 0;
    };
    schedule
        .upcoming(Utc)
        .next()
        .map(|dt| dt.timestamp_micros())
        .unwrap_or(0)
}

/// Normalise to the `cron` crate's expected 6+ field form. A 5-field expr
/// is treated as "minute-precision" and gets a `0 ` seconds prefix.
fn normalize_cron(expr: &str) -> String {
    let fields = expr.split_whitespace().count();
    if fields == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

/// Spawn a scheduler loop for one schedule. Cancellable via the returned
/// token (drop the token to leave the task running; call `.cancel()` to
/// stop it cleanly). The submit_fn closure encapsulates how to actually
/// dispatch a task (the daemon passes its `submit_task` machinery).
pub fn spawn_loop<F>(record: ScheduleRecord, ledger: Ledger, submit_fn: F) -> CancellationToken
where
    F: Fn(ScheduleRecord) -> tokio::task::JoinHandle<Result<String, String>>
        + Send
        + Sync
        + 'static,
{
    let cancel = CancellationToken::new();
    let cancel_child = cancel.clone();
    let id = record.id.clone();
    tokio::spawn(async move {
        loop {
            // Refetch the record on every tick so paused/cron edits take
            // effect without a restart.
            let current = match ledger.get_schedule(&id).await {
                Ok(r) => r,
                Err(e) => {
                    debug!(%id, error = %e, "scheduler: schedule disappeared, stopping loop");
                    return;
                }
            };
            if current.paused {
                debug!(%id, "scheduler: paused, sleeping 30s");
                tokio::select! {
                    _ = cancel_child.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(30)) => continue,
                }
            }
            let next_us = next_run_micros(&current.cron);
            if next_us <= 0 {
                warn!(%id, cron = %current.cron, "scheduler: no future occurrence; sleeping 1h");
                tokio::select! {
                    _ = cancel_child.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(3600)) => continue,
                }
            }
            // Publish next_run_micros so the SPA's upcoming-runs panel is
            // accurate even while we're sleeping.
            let _ = ledger.set_schedule_next_run(&id, next_us).await;
            let now_us = Utc::now().timestamp_micros();
            let wait_us = (next_us - now_us).max(0);
            let wait = Duration::from_micros(wait_us as u64);
            debug!(%id, wait_s = wait.as_secs(), "scheduler: sleeping until next run");
            tokio::select! {
                _ = cancel_child.cancelled() => return,
                _ = tokio::time::sleep(wait) => {}
            }
            // Fire.
            info!(%id, cron = %current.cron, "scheduler: firing");
            let handle = submit_fn(current.clone());
            match handle.await {
                Ok(Ok(task_id)) => {
                    let after_next = next_run_micros(&current.cron);
                    if let Err(e) = ledger.record_schedule_fire(&id, &task_id, after_next).await {
                        warn!(%id, error = %e, "scheduler: record_schedule_fire failed");
                    }
                }
                Ok(Err(e)) => warn!(%id, error = %e, "scheduler: submit returned error"),
                Err(e) => warn!(%id, error = %e, "scheduler: submit task join error"),
            }
            // Tiny delay so a misconfigured cron like "* * * * * *" doesn't
            // hot-loop.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    cancel
}

/// Convenience: turn a fresh API spec into the ledger `NewSchedule`. Cron
/// validation is the caller's responsibility (do it before persisting).
#[allow(clippy::too_many_arguments)] // mirrors the proto-derived ScheduleSpec
pub fn new_record_from_spec(
    id: String,
    cron: String,
    goal: String,
    workdir: String,
    sandbox: String,
    net_policy: String,
    routing_policy: String,
    max_steps: u32,
    label: String,
    paused: bool,
) -> NewSchedule {
    NewSchedule {
        id,
        cron,
        goal,
        workdir,
        sandbox,
        net_policy,
        routing_policy,
        max_steps,
        label,
        paused,
    }
}

/// Pretty-print a `next_run_micros` for human-readable logs.
#[allow(dead_code)]
pub fn format_next_run(micros: i64) -> String {
    if micros <= 0 {
        return "—".to_string();
    }
    Utc.timestamp_opt(micros / 1_000_000, ((micros % 1_000_000) * 1_000) as u32)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_5_field_cron() {
        // Classic unix cron: minute hour dom mon dow.
        assert!(validate_cron("0 0 * * *").is_ok());
        assert!(validate_cron("*/5 * * * *").is_ok());
    }

    #[test]
    fn validates_6_field_cron() {
        assert!(validate_cron("0 0 0 * * *").is_ok()); // every midnight
        assert!(validate_cron("0 */5 * * * *").is_ok()); // every 5 minutes
    }

    #[test]
    fn rejects_garbage() {
        assert!(validate_cron("not a cron").is_err());
        assert!(validate_cron("").is_err());
    }

    #[test]
    fn next_run_in_future() {
        let n = next_run_micros("0 */5 * * * *");
        let now = Utc::now().timestamp_micros();
        assert!(n > now);
    }
}
