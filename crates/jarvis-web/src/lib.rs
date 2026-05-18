//! Jarvis HTTP/web layer.
//!
//! Hosts three concerns under one axum server:
//! - `connect` — Connect protocol (tonic-web) bridge to the gRPC API for the SolidJS SPA.
//! - `sse` — high-throughput Server-Sent Events stream of ledger events.
//! - SPA static assets — the SolidJS bundle embedded via rust-embed.
//!
//! The legacy HTMX UI continues to live in `jarvis-daemon/src/web/` during the
//! M6/M7 transition and serves on its own port (`cfg.web.addr`, default 7878).
//! This crate serves on a separate port (`JARVIS_SPA_ADDR`, default 7879) and
//! consumes the daemon ONLY through the public gRPC API — no internal types.

#![forbid(unsafe_code)]

pub mod server;

pub use server::{resolve_addr, router, serve, DEFAULT_SPA_ADDR};

/// Crate version, mirrors workspace.package.version. Surfaced over the web API so
/// the SPA can detect a backend upgrade and prompt a reload.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
