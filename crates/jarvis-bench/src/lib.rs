//! jarvis-bench — reproducible benchmark harness for Jarvis.
//!
//! Replays a declarative YAML suite of tasks against a config/model and
//! emits a JSON scorecard. The bench is a pure consumer of the public
//! agent API (`jarvis_agent::run_agent`); it neither modifies the daemon
//! nor touches the agent loop.

pub mod runner;
pub mod scorecard;
pub mod stub_provider;
pub mod suite;

pub use runner::{RunOptions, run_task};
pub use scorecard::{ConfigSummary, Scorecard, ScorecardSummary, TaskOutcome, TaskResult};
pub use stub_provider::StubProvider;
pub use suite::{Defaults, SuccessCriterion, Suite, Task};
