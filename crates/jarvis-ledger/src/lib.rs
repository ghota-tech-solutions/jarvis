//! jarvis-ledger — append-only SQLite event store.
//!
//! Single writer (the daemon's owning task), many readers. WAL mode + busy_timeout
//! for read concurrency. UPDATE/DELETE on the events table are blocked by trigger.
//!
//! Payloads are JSON strings in M2 (debuggable). May migrate to CBOR later.

mod model;
mod store;

pub use model::{EventKind, EventRecord, NewEvent, TaskRecord, TaskRuntimeInfo, TaskStatus};
pub use store::{Ledger, LedgerError};
