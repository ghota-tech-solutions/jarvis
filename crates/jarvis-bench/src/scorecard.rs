//! Scorecard — per-task results + aggregate summary, serialized as JSON.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Pass,
    Fail,
    Timeout,
    Error,
}

impl TaskOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Timeout => "timeout",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub id: String,
    pub outcome: TaskOutcome,
    pub steps_taken: u32,
    pub duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScorecardSummary {
    pub total: u32,
    pub pass: u32,
    pub fail: u32,
    pub timeout: u32,
    pub error: u32,
    pub pass_rate: f64,
    pub total_duration_ms: u64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSummary {
    pub model: String,
    pub routing: String,
    pub sandbox: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scorecard {
    pub suite: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub config_summary: ConfigSummary,
    pub tasks: Vec<TaskResult>,
    pub summary: ScorecardSummary,
}

impl Scorecard {
    pub fn new(
        suite: String,
        started_at: DateTime<Utc>,
        config_summary: ConfigSummary,
        tasks: Vec<TaskResult>,
    ) -> Self {
        let finished_at = Utc::now();
        let summary = ScorecardSummary::from_tasks(&tasks);
        Self {
            suite,
            started_at,
            finished_at,
            config_summary,
            tasks,
            summary,
        }
    }
}

impl ScorecardSummary {
    pub fn from_tasks(tasks: &[TaskResult]) -> Self {
        let total = tasks.len() as u32;
        let mut s = Self {
            total,
            ..Self::default()
        };
        for t in tasks {
            match t.outcome {
                TaskOutcome::Pass => s.pass += 1,
                TaskOutcome::Fail => s.fail += 1,
                TaskOutcome::Timeout => s.timeout += 1,
                TaskOutcome::Error => s.error += 1,
            }
            s.total_duration_ms += t.duration_ms;
            s.total_tokens_in += t.tokens_in;
            s.total_tokens_out += t.tokens_out;
            s.total_cost_usd += t.cost_usd;
        }
        s.pass_rate = if total == 0 {
            0.0
        } else {
            f64::from(s.pass) / f64::from(total)
        };
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scorecard_serialization_roundtrip() {
        let card = Scorecard::new(
            "basic".to_string(),
            Utc::now(),
            ConfigSummary {
                model: "stub".into(),
                routing: "auto".into(),
                sandbox: "native".into(),
            },
            vec![TaskResult {
                id: "t1".into(),
                outcome: TaskOutcome::Pass,
                steps_taken: 2,
                duration_ms: 100,
                tokens_in: 10,
                tokens_out: 5,
                cost_usd: 0.0,
                failure_reason: None,
            }],
        );
        let s = serde_json::to_string(&card).unwrap();
        let back: Scorecard = serde_json::from_str(&s).unwrap();
        assert_eq!(back.tasks.len(), 1);
        assert_eq!(back.summary.pass, 1);
        assert!((back.summary.pass_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn summary_aggregates_outcomes_and_tokens() {
        let tasks = vec![
            TaskResult {
                id: "a".into(),
                outcome: TaskOutcome::Pass,
                steps_taken: 1,
                duration_ms: 100,
                tokens_in: 10,
                tokens_out: 1,
                cost_usd: 0.0,
                failure_reason: None,
            },
            TaskResult {
                id: "b".into(),
                outcome: TaskOutcome::Fail,
                steps_taken: 1,
                duration_ms: 200,
                tokens_in: 20,
                tokens_out: 2,
                cost_usd: 0.0,
                failure_reason: Some("nope".into()),
            },
            TaskResult {
                id: "c".into(),
                outcome: TaskOutcome::Timeout,
                steps_taken: 5,
                duration_ms: 1000,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                failure_reason: Some("timeout".into()),
            },
        ];
        let s = ScorecardSummary::from_tasks(&tasks);
        assert_eq!(s.total, 3);
        assert_eq!(s.pass, 1);
        assert_eq!(s.fail, 1);
        assert_eq!(s.timeout, 1);
        assert_eq!(s.error, 0);
        assert_eq!(s.total_duration_ms, 1300);
        assert_eq!(s.total_tokens_in, 30);
        assert_eq!(s.total_tokens_out, 3);
        assert!((s.pass_rate - (1.0 / 3.0)).abs() < 1e-9);
    }
}
