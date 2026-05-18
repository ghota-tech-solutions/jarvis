// Daemon sidecar management.
//
// On first launch (and on the user's request via the `start_daemon`
// command), spawn `jarvis-daemon` as a child process. The daemon writes
// its bearer token to `<data_dir>/web.token`; we surface it to the SPA
// via the `read_web_token` IPC so the user never sees the URL fragment.
//
// Production builds ship the daemon binary as a Tauri "sidecar". In
// dev, we look for it in `target/debug/jarvis-daemon` first, then fall
// back to PATH.

use anyhow::{Context as _, Result};
use serde::Serialize;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tracing::{info, warn};

const DAEMON_ADDR: &str = "127.0.0.1:7777";

#[derive(Serialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub addr: String,
}

pub async fn ensure_daemon_running(app: &AppHandle) -> Result<()> {
    if is_daemon_up().await {
        info!("daemon already running at {DAEMON_ADDR}");
        return Ok(());
    }
    spawn_daemon(app).await
}

async fn is_daemon_up() -> bool {
    tokio::task::spawn_blocking(|| {
        TcpStream::connect_timeout(
            &DAEMON_ADDR.parse().unwrap(),
            Duration::from_millis(300),
        )
        .is_ok()
    })
    .await
    .unwrap_or(false)
}

async fn spawn_daemon(_app: &AppHandle) -> Result<()> {
    let bin = locate_daemon_binary().context("locate jarvis-daemon binary")?;
    info!(path = %bin.display(), "spawning daemon sidecar");
    tokio::process::Command::new(&bin)
        .arg("run")
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;
    // Poll for readiness up to ~3 s
    for _ in 0..30 {
        if is_daemon_up().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    warn!("daemon spawn timeout — proceeding anyway");
    Ok(())
}

fn locate_daemon_binary() -> Option<PathBuf> {
    // Dev: same workspace target dir.
    let candidates = [
        // Workspace target (relative to crate root during dev)
        PathBuf::from("../../target/debug/jarvis-daemon.exe"),
        PathBuf::from("../../target/debug/jarvis-daemon"),
        PathBuf::from("../../target/release/jarvis-daemon.exe"),
        PathBuf::from("../../target/release/jarvis-daemon"),
        // Sidecar (production install)
        PathBuf::from("jarvis-daemon.exe"),
        PathBuf::from("jarvis-daemon"),
    ];
    for c in candidates {
        if c.exists() {
            return Some(c);
        }
    }
    // Fall back to PATH lookup
    which::which("jarvis-daemon").ok()
}

#[tauri::command]
pub async fn daemon_status() -> DaemonStatus {
    DaemonStatus {
        running: is_daemon_up().await,
        addr: DAEMON_ADDR.to_string(),
    }
}

#[tauri::command]
pub async fn start_daemon(app: AppHandle) -> Result<DaemonStatus, String> {
    ensure_daemon_running(&app)
        .await
        .map_err(|e| e.to_string())?;
    Ok(DaemonStatus {
        running: is_daemon_up().await,
        addr: DAEMON_ADDR.to_string(),
    })
}

#[tauri::command]
pub async fn stop_daemon() -> Result<(), String> {
    // Best-effort: we don't track the PID we spawned. The user can kill
    // the daemon process from the OS task manager; this command exists
    // for parity with the SPA's UX, even if it currently no-ops in dev.
    Ok(())
}

/// Read the bearer token from `<data_dir>/web.token`. The SPA can then
/// pre-populate sessionStorage without exposing the URL fragment.
#[tauri::command]
pub async fn read_web_token(app: AppHandle) -> Result<String, String> {
    let dirs = app.path();
    // Match jarvis-daemon's default `data_dir = ".jarvis"`.
    // In a Tauri-packaged install, the daemon's data_dir lives next to
    // the executable. For dev, we look in the current working dir.
    let candidates = [
        dirs.app_data_dir().unwrap_or_default().join("web.token"),
        PathBuf::from(".jarvis/web.token"),
        PathBuf::from("../../.jarvis/web.token"),
    ];
    for c in candidates {
        if let Ok(s) = std::fs::read_to_string(&c) {
            return Ok(s.trim().to_string());
        }
    }
    Err("web.token not found".into())
}
