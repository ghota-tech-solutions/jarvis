//! § T2.10 — file-system watcher feeding the `WatchWorkdir` gRPC stream.
//!
//! Wraps `notify-debouncer-mini` (which itself sits on top of `notify`)
//! so a noisy editor save burst (multiple sub-second writes to the same
//! file) collapses into a single `FsEvent`. The watcher runs on a
//! background tokio task; events land on a bounded mpsc channel sized
//! for the stream, and the channel sender uses `try_send` so a slow
//! client can never block the watcher thread — drops are silent and
//! logged at `debug`.
//!
//! v1 surface is observability only — the agent does NOT yet
//! auto-preempt running tasks on these events. That's v2 with policy
//! gating + a verdict-style "should I interrupt?" check.

use anyhow::{Context, Result};
use jarvis_api::FsEvent as ApiFsEvent;
use notify::{EventKind, RecursiveMode};
use notify_debouncer_mini::{DebouncedEventKind, new_debouncer_opt};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Default debounce window. Editors that save-on-every-keystroke (e.g.
/// VS Code with auto-save) produce dozens of events per second on the
/// same file; 100 ms compacts them without making interactive flows feel
/// laggy.
const DEBOUNCE_MS: u64 = 100;

/// Mailbox size for the watcher → gRPC bridge. Generous enough that a
/// large refactor (`cargo fmt --all`) doesn't blow it out, small enough
/// that a stalled client doesn't grow unbounded memory.
pub const FS_EVENT_BUFFER: usize = 512;

fn now_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

fn classify(kind: DebouncedEventKind) -> &'static str {
    match kind {
        // The mini debouncer only emits `Any` / `AnyContinuous` (and
        // ignores `OnError` paths internally). We map both to `modified`
        // since the v1 SPA doesn't care about the distinction — the
        // detail-fidelity work moves to v2 if we ever need a separate
        // `created` / `removed` lane.
        DebouncedEventKind::Any => "modified",
        DebouncedEventKind::AnyContinuous => "modified",
        _ => "other",
    }
}

#[allow(dead_code)]
fn classify_raw(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "created",
        EventKind::Modify(_) => "modified",
        EventKind::Remove(_) => "removed",
        _ => "other",
    }
}

/// Spawn a watcher on `workdir` and return a receiver that yields
/// `ApiFsEvent`s ready to be forwarded over the gRPC stream. The watcher
/// runs until both the receiver is dropped AND the inner notify watcher
/// is dropped (returned alongside the receiver so the caller controls
/// lifetime — drop the handle to stop watching).
pub fn spawn(
    workdir: &Path,
) -> Result<(
    mpsc::Receiver<ApiFsEvent>,
    notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
)> {
    if !workdir.is_dir() {
        return Err(anyhow::anyhow!(
            "workdir is not a directory: {}",
            workdir.display()
        ));
    }
    let root = workdir.to_path_buf();
    let (tx, rx) = mpsc::channel::<ApiFsEvent>(FS_EVENT_BUFFER);

    let cfg = notify_debouncer_mini::Config::default()
        .with_timeout(Duration::from_millis(DEBOUNCE_MS))
        .with_notify_config(notify::Config::default());

    let mut debouncer = new_debouncer_opt::<_, notify::RecommendedWatcher>(
        cfg,
        move |res: notify_debouncer_mini::DebounceEventResult| {
            let events = match res {
                Ok(evts) => evts,
                Err(e) => {
                    warn!(error = ?e, "file_watch: notify error");
                    return;
                }
            };
            for ev in events {
                let rel =
                    relative(&ev.path, &root).unwrap_or_else(|| ev.path.display().to_string());
                let api_ev = ApiFsEvent {
                    path: rel,
                    kind: classify(ev.kind).to_string(),
                    ts_micros: now_micros(),
                };
                if let Err(e) = tx.try_send(api_ev) {
                    // `try_send` fails on `Full` (drop the event — client
                    // is too slow) or `Closed` (receiver gone — caller
                    // dropped the stream, nothing to do).
                    debug!(error = ?e, "file_watch: dropping event");
                }
            }
        },
    )
    .context("file_watch: failed to create debouncer")?;

    debouncer
        .watcher()
        .watch(workdir, RecursiveMode::Recursive)
        .context("file_watch: failed to start watching")?;

    Ok((rx, debouncer))
}

fn relative(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn classify_returns_modified_for_any() {
        assert_eq!(classify(DebouncedEventKind::Any), "modified");
        assert_eq!(classify(DebouncedEventKind::AnyContinuous), "modified");
    }

    #[test]
    fn relative_strips_root_and_normalises_separators() {
        let root = PathBuf::from("/tmp/work");
        let path = PathBuf::from("/tmp/work/src/main.rs");
        assert_eq!(relative(&path, &root), Some("src/main.rs".to_string()));
    }

    #[tokio::test]
    async fn spawn_rejects_non_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("not-a-dir.txt");
        fs::write(&file, "x").unwrap();
        assert!(spawn(&file).is_err());
    }

    #[tokio::test]
    async fn spawn_emits_event_on_file_write() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut rx, _debouncer) = spawn(tmp.path()).expect("spawn ok");
        // Give the watcher a beat to register.
        tokio::time::sleep(Duration::from_millis(120)).await;
        fs::write(tmp.path().join("hello.txt"), "hi").unwrap();
        // Wait up to 2s for debounced flush.
        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event arrived")
            .expect("channel open");
        assert!(got.path.ends_with("hello.txt"));
        assert!(!got.kind.is_empty());
    }
}
