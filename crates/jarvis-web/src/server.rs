//! Axum server boot for the new SPA layer.
//!
//! Runs on its own port, in parallel to the legacy HTMX UI in
//! `jarvis-daemon/src/web/`. Coexistence is the M6 design: the legacy keeps
//! serving on `cfg.web.addr` (default 7878), the new SPA layer serves on
//! `spa_addr` (default 7879, override via env `JARVIS_SPA_ADDR`).
//!
//! At M6.S3 the only route is `/v1/ping` — a smoke test that proves wiring.
//! Subsequent steps add Connect (gRPC-Web bridge), SSE event stream,
//! auth middleware, and the embedded SolidJS bundle.

use anyhow::Context as _;
use axum::{routing::get, Json, Router};
use serde::Serialize;
use tracing::info;

use crate::VERSION;

/// Default bind address when `JARVIS_SPA_ADDR` is unset.
pub const DEFAULT_SPA_ADDR: &str = "127.0.0.1:7879";

/// Resolve the SPA bind address: env override, or fallback to default.
pub fn resolve_addr() -> String {
    std::env::var("JARVIS_SPA_ADDR").unwrap_or_else(|_| DEFAULT_SPA_ADDR.to_string())
}

/// Build the axum router for the SPA layer.
///
/// At M6.S3 this is just a health check. Subsequent steps add Connect/SSE/static
/// routes by chaining `.merge(...)` calls.
pub fn router() -> Router {
    Router::new().route("/v1/ping", get(ping))
}

#[derive(Serialize)]
struct PingResponse {
    ok: bool,
    crate_: &'static str,
    version: &'static str,
}

async fn ping() -> Json<PingResponse> {
    Json(PingResponse {
        ok: true,
        crate_: "jarvis-web",
        version: VERSION,
    })
}

/// Run the SPA axum server until the future is dropped or the listener errors.
pub async fn serve(addr: String) -> anyhow::Result<()> {
    let app = router();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind jarvis-web SPA on {addr}"))?;
    info!(%addr, "jarvis-web SPA listening");
    axum::serve(listener, app)
        .await
        .context("jarvis-web SPA server")
}
