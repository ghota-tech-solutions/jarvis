// Windows: suppress the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sidecar;

use tracing::warn;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("JARVIS_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .setup(|app| {
            // Spawn the daemon as a sidecar process if it isn't already running.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = sidecar::ensure_daemon_running(&app_handle).await {
                    warn!(error = %e, "could not start daemon sidecar");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            sidecar::daemon_status,
            sidecar::stop_daemon,
            sidecar::start_daemon,
            sidecar::read_web_token,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
