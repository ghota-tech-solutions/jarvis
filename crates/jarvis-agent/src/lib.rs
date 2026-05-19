//! jarvis-agent — single-task plan-act-observe loop.
//!
//! The agent receives a goal + workdir, talks to a single `LlmProvider` (M5 swaps
//! this for the router), invokes tools from a `ToolRegistry`, and logs every step
//! to the ledger. The loop terminates on one of:
//!
//! - the LLM emits `"action": "done"` (or the `<<DONE>>` sentinel),
//! - the step budget is exhausted,
//! - the cancellation token fires,
//! - an unrecoverable error.

mod loop_;
pub mod memory_extractor;
mod prompt;
mod protocol;
pub mod validator;

pub use loop_::{AgentRun, HookSpec, ValidationSpec, run_agent};
pub use memory_extractor::extract_for_task;
pub use protocol::{AgentError, Outcome};
