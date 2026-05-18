//! Jarvis HTTP/web layer.
//!
//! Hosts three concerns under one axum server:
//! - `connect` — Connect protocol (tonic-web) bridge to the gRPC API for the SolidJS SPA.
//! - `sse` — high-throughput Server-Sent Events stream of ledger events.
//! - `legacy` — the existing HTMX UI, kept mounted at `/legacy/*` during the M6/M7 transition.
//!
//! The crate is intentionally a separate workspace member so the daemon binary
//! does not need to pull in SPA assets (rust-embed) or Connect machinery when
//! `--no-default-features --features grpc-only` is selected.

#![forbid(unsafe_code)]

/// Crate version, mirrors workspace.package.version. Surfaced over the web API so
/// the SPA can detect a backend upgrade and prompt a reload.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
