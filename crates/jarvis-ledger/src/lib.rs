//! jarvis-ledger — append-only SQLite event store.
//!
//! Single writer (the daemon's owning task), many readers. WAL mode + busy_timeout
//! for read concurrency. UPDATE/DELETE on the events table are blocked by trigger.
//!
//! Payloads are JSON strings in M2 (debuggable). May migrate to CBOR later.

pub mod memory;
mod model;
pub mod schedule;
mod store;

pub use memory::{MemoryKind, MemoryLayer, MemoryRecord, MemoryScope, MemoryStatus, NewMemory};
pub use model::{EventKind, EventRecord, NewEvent, TaskRecord, TaskRuntimeInfo, TaskStatus};
pub use schedule::{NewSchedule, ScheduleRecord};
pub use store::{Ledger, LedgerError};
