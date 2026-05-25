//! Suite parsing — YAML → typed types + success criterion evaluation.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A bench suite is a named, ordered list of tasks with shared defaults.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Suite {
    pub suite: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub defaults: Defaults,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Defaults {
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    #[serde(default = "default_timeout_s")]
    pub timeout_s: u64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            max_steps: default_max_steps(),
            timeout_s: default_timeout_s(),
        }
    }
}

fn default_max_steps() -> u32 {
    10
}
fn default_timeout_s() -> u64 {
    120
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Task {
    pub id: String,
    pub goal: String,
    /// Files to seed in the tempdir before the agent runs.
    /// Key: relative path. Value: file contents.
    #[serde(default)]
    pub fixtures: BTreeMap<String, String>,
    pub success: SuccessCriterion,
    /// Per-task override of defaults.
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Predicates evaluated on the tempdir after the agent loop returns.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SuccessCriterion {
    /// File exists and its trimmed content equals `content` (also trimmed).
    FileEquals { path: String, content: String },
    /// File exists and contains `pattern` as a substring.
    FileContains { path: String, pattern: String },
    /// Shell command run in the workdir; success means exit code 0.
    ShellExitsZero { cmd: String },
}

/// The outcome of evaluating a criterion.
#[derive(Debug, Clone)]
pub enum CriterionResult {
    Pass,
    Fail { reason: String },
}

impl Suite {
    /// Load a suite from a YAML file on disk.
    pub fn from_path(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading suite file `{}`", path.display()))?;
        Self::from_yaml_str(&raw)
    }

    /// Parse a suite from a YAML string.
    pub fn from_yaml_str(raw: &str) -> Result<Self> {
        let s: Self = serde_yaml::from_str(raw).context("parsing suite YAML")?;
        s.validate()?;
        Ok(s)
    }

    fn validate(&self) -> Result<()> {
        if self.suite.trim().is_empty() {
            return Err(anyhow!("suite name must be non-empty"));
        }
        if self.tasks.is_empty() {
            return Err(anyhow!("suite must contain at least one task"));
        }
        let mut seen = std::collections::HashSet::new();
        for t in &self.tasks {
            if t.id.trim().is_empty() {
                return Err(anyhow!("task id must be non-empty"));
            }
            if !seen.insert(&t.id) {
                return Err(anyhow!("duplicate task id `{}`", t.id));
            }
            if t.goal.trim().is_empty() {
                return Err(anyhow!("task `{}` has empty goal", t.id));
            }
        }
        Ok(())
    }
}

impl Task {
    pub fn effective_max_steps(&self, defaults: &Defaults) -> u32 {
        self.max_steps.unwrap_or(defaults.max_steps)
    }

    pub fn effective_timeout_s(&self, defaults: &Defaults) -> u64 {
        self.timeout_s.unwrap_or(defaults.timeout_s)
    }
}

impl SuccessCriterion {
    /// Evaluate the criterion against `workdir`. Pure I/O; no LLM.
    pub async fn evaluate(&self, workdir: &Path) -> CriterionResult {
        match self {
            Self::FileEquals { path, content } => evaluate_file_equals(workdir, path, content),
            Self::FileContains { path, pattern } => evaluate_file_contains(workdir, path, pattern),
            Self::ShellExitsZero { cmd } => evaluate_shell_exits_zero(workdir, cmd).await,
        }
    }
}

fn evaluate_file_equals(workdir: &Path, rel: &str, expected: &str) -> CriterionResult {
    let p = resolve_under(workdir, rel);
    match std::fs::read_to_string(&p) {
        Ok(actual) => {
            if actual.trim() == expected.trim() {
                CriterionResult::Pass
            } else {
                CriterionResult::Fail {
                    reason: format!(
                        "file_equals: `{}` content mismatch (got {} bytes)",
                        rel,
                        actual.len()
                    ),
                }
            }
        }
        Err(e) => CriterionResult::Fail {
            reason: format!("file_equals: cannot read `{}`: {e}", p.display()),
        },
    }
}

fn evaluate_file_contains(workdir: &Path, rel: &str, pattern: &str) -> CriterionResult {
    let p = resolve_under(workdir, rel);
    match std::fs::read_to_string(&p) {
        Ok(actual) => {
            if actual.contains(pattern) {
                CriterionResult::Pass
            } else {
                CriterionResult::Fail {
                    reason: format!(
                        "file_contains: `{}` does not contain `{}` (file is {} bytes)",
                        rel,
                        pattern,
                        actual.len()
                    ),
                }
            }
        }
        Err(e) => CriterionResult::Fail {
            reason: format!("file_contains: cannot read `{}`: {e}", p.display()),
        },
    }
}

async fn evaluate_shell_exits_zero(workdir: &Path, cmd: &str) -> CriterionResult {
    // Use the same `cmd /C` (Windows) / `sh -c` (unix) shape jarvis-sandbox uses
    // for shell, so the bench mirrors agent behavior. We don't pull the sandbox
    // crate here — the evaluator only needs exit-code semantics.
    #[cfg(windows)]
    let mut command = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };

    command.current_dir(workdir);
    let output = match command.output().await {
        Ok(o) => o,
        Err(e) => {
            return CriterionResult::Fail {
                reason: format!("shell_exits_zero: spawn failed: {e}"),
            };
        }
    };
    if output.status.success() {
        CriterionResult::Pass
    } else {
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".to_string());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.chars().rev().take(200).collect::<String>();
        let tail: String = tail.chars().rev().collect();
        CriterionResult::Fail {
            reason: format!("shell_exits_zero: exit={code} stderr_tail=`{tail}`"),
        }
    }
}

/// Best-effort confine a user-supplied relative path under `workdir`.
/// Absolute paths and `..` segments are tolerated here (the bench may want to
/// validate state outside the workdir in the future); for v0 we just join.
fn resolve_under(workdir: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workdir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_suite() {
        let yaml = include_str!("../benches/basic.yaml");
        let s = Suite::from_yaml_str(yaml).expect("parse");
        assert_eq!(s.suite, "basic");
        assert_eq!(s.tasks.len(), 3);
        assert_eq!(s.tasks[0].id, "write-hello");
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let yaml = r#"
suite: dup
tasks:
  - id: a
    goal: g
    success: { type: file_equals, path: x, content: y }
  - id: a
    goal: g
    success: { type: file_equals, path: x, content: y }
"#;
        assert!(Suite::from_yaml_str(yaml).is_err());
    }

    #[test]
    fn validate_rejects_empty_tasks() {
        let yaml = "suite: empty\ntasks: []\n";
        assert!(Suite::from_yaml_str(yaml).is_err());
    }

    #[tokio::test]
    async fn success_criterion_file_equals() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi\n").unwrap();
        let c = SuccessCriterion::FileEquals {
            path: "hello.txt".into(),
            content: "hi".into(),
        };
        assert!(matches!(
            c.evaluate(dir.path()).await,
            CriterionResult::Pass
        ));
    }

    #[tokio::test]
    async fn success_criterion_file_equals_fails_on_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "wrong").unwrap();
        let c = SuccessCriterion::FileEquals {
            path: "hello.txt".into(),
            content: "hi".into(),
        };
        assert!(matches!(
            c.evaluate(dir.path()).await,
            CriterionResult::Fail { .. }
        ));
    }

    #[tokio::test]
    async fn success_criterion_file_contains() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.rs"), "pub fn bar() {}").unwrap();
        let c = SuccessCriterion::FileContains {
            path: "foo.rs".into(),
            pattern: "pub fn bar".into(),
        };
        assert!(matches!(
            c.evaluate(dir.path()).await,
            CriterionResult::Pass
        ));
    }

    #[tokio::test]
    async fn success_criterion_shell_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let c = SuccessCriterion::ShellExitsZero {
            cmd: "echo done".into(),
        };
        assert!(matches!(
            c.evaluate(dir.path()).await,
            CriterionResult::Pass
        ));
    }

    #[tokio::test]
    async fn success_criterion_shell_exits_zero_fails_on_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        // `exit 1` works in both cmd.exe and sh.
        let c = SuccessCriterion::ShellExitsZero {
            cmd: "exit 1".into(),
        };
        assert!(matches!(
            c.evaluate(dir.path()).await,
            CriterionResult::Fail { .. }
        ));
    }
}
