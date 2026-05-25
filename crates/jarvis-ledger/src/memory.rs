//! Long-lived memories — facts the user has promoted from the agent's
//! candidate proposals. The agent loop reads these for the current workdir
//! (plus all `scope='global'` memories) and injects them into the system
//! prompt at the start of every task.
//!
//! Schema lives in `migrations/0001_initial.sql` (`memories` table). Status transitions:
//!   `candidate` → user calls PromoteMemory → `active`
//!   `active`    → user calls ForgetMemory  → `forgotten`
//!   `candidate` → user calls ForgetMemory  → `forgotten`
//! Forgotten rows are kept so future extractor runs can deduplicate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// Applies only to tasks whose workdir matches `scope_value`.
    Workdir,
    /// Applies to every task.
    Global,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workdir => "workdir",
            Self::Global => "global",
        }
    }
}

impl std::str::FromStr for MemoryScope {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "workdir" => Self::Workdir,
            "global" => Self::Global,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// "the agent should retry shell commands on transient network errors"
    Pattern,
    /// "user prefers French in commit messages"
    Preference,
    /// "this project's main branch is `develop`, not `main`"
    Fact,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Preference => "preference",
            Self::Fact => "fact",
        }
    }
}

impl std::str::FromStr for MemoryKind {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "pattern" => Self::Pattern,
            "preference" => Self::Preference,
            "fact" => Self::Fact,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Candidate,
    Active,
    Forgotten,
}

impl MemoryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active => "active",
            Self::Forgotten => "forgotten",
        }
    }
}

impl std::str::FromStr for MemoryStatus {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "candidate" => Self::Candidate,
            "active" => Self::Active,
            "forgotten" => Self::Forgotten,
            _ => return Err(()),
        })
    }
}

/// § T2.1 — Memory layer (Hermes-style five-tier scheme). Each layer
/// has its own retention + recall policy applied at prompt-assembly:
///
/// - **Working** — turn-local: produced mid-task, evicted at task end.
///   Reserved for a future v2 of the extractor; not used in v1.
/// - **Episodic** — task-local: events / outcomes specific to one task
///   ("retried `cargo test` 3 times before noticing flaky test X").
///   Recall is recency-biased and capped tight (≤ N most recent).
/// - **Semantic** — cross-task patterns ("for rust async, prefer
///   `tokio::select!` over manually polling futures"). Always rendered
///   when active. This is the default for memories extracted today.
/// - **Procedural** — step-by-step procedures the agent learned ("to
///   add a new RPC: edit proto → regen TS → update service.rs →
///   register handler"). Always rendered. Larger budget than
///   semantic so multi-step procedures aren't truncated.
/// - **Archival** — older memories that survived but are no longer
///   first-tier. Compressed renderer or skipped entirely depending
///   on prompt budget; the source of truth lives on disk for
///   forensic queries (`/memory?layer=archival`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLayer {
    Working,
    Episodic,
    #[default]
    Semantic,
    Procedural,
    Archival,
}

impl MemoryLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
            Self::Archival => "archival",
        }
    }
}

impl std::str::FromStr for MemoryLayer {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "working" => Self::Working,
            "episodic" => Self::Episodic,
            "semantic" => Self::Semantic,
            "procedural" => Self::Procedural,
            "archival" => Self::Archival,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: i64,
    pub scope: MemoryScope,
    pub scope_value: String,
    pub kind: MemoryKind,
    pub layer: MemoryLayer,
    pub text: String,
    pub status: MemoryStatus,
    pub source_task_id: Option<jarvis_core::TaskId>,
    pub created_at: i64,
    pub updated_at: i64,
    pub usage_count: i64,
}

#[derive(Debug, Clone)]
pub struct NewMemory {
    pub scope: MemoryScope,
    pub scope_value: String,
    pub kind: MemoryKind,
    pub layer: MemoryLayer,
    pub text: String,
    pub status: MemoryStatus,
    pub source_task_id: Option<jarvis_core::TaskId>,
}
