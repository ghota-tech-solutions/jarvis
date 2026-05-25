// Auxiliary IPC commands not tied to the daemon sidecar lifecycle.
//
// `cancel_all_running` is invoked from the tray menu and (eventually)
// from the SPA. In v1 it's a stub that logs intent — the daemon already
// exposes a `CancelTask(TaskHandle)` gRPC, but jarvis-desktop doesn't
// link tonic/jarvis-api yet. A follow-up will wire the gRPC client.

use tauri::{AppHandle, Runtime};
use tracing::info;

/// Cancel every currently running task on the daemon.
///
/// TODO(F1.x): replace the stub with a real tonic client call:
///   1. `ListTasks(filter=Running)`
///   2. for each handle, `CancelTask(handle)`
///   3. surface per-task results back to the UI.
#[tauri::command]
pub async fn cancel_all_running_cmd<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    cancel_all_running(app).await.map_err(|e| e.to_string())
}

pub async fn cancel_all_running<R: Runtime>(_app: AppHandle<R>) -> anyhow::Result<()> {
    info!(
        "cancel_all_running: STUB — no gRPC client wired yet, would call \
         daemon.ListTasks(Running) + CancelTask(...) for each"
    );
    Ok(())
}
