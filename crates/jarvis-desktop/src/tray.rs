// System tray icon + menu.
//
// The tray is the primary way to bring the window back once the user
// clicks the X button (which now hides the window rather than killing
// the process — see `main.rs` `WindowEvent::CloseRequested` handler).
//
// Menu layout:
//   Show / Hide Jarvis
//   ───────────────────
//   Daemon (status)        [disabled]
//   Cancel all running tasks
//   ───────────────────
//   Quit Jarvis
//
// The Quit item is the only path that actually terminates the process.

use tauri::{
    AppHandle, Manager, Runtime,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconBuilder,
};
use tracing::{info, warn};

const MENU_ID_TOGGLE: &str = "toggle";
const MENU_ID_DAEMON_STATUS: &str = "daemon_status";
const MENU_ID_CANCEL_ALL: &str = "cancel_all";
const MENU_ID_QUIT: &str = "quit";

/// Build the tray icon and install it on the app. Call from `setup`.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let toggle = MenuItemBuilder::with_id(MENU_ID_TOGGLE, "Show / Hide Jarvis").build(app)?;
    let daemon_status = MenuItemBuilder::with_id(MENU_ID_DAEMON_STATUS, "Daemon: starting…")
        .enabled(false)
        .build(app)?;
    let cancel_all =
        MenuItemBuilder::with_id(MENU_ID_CANCEL_ALL, "Cancel all running tasks").build(app)?;
    let quit = MenuItemBuilder::with_id(MENU_ID_QUIT, "Quit Jarvis").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&toggle)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&daemon_status)
        .item(&cancel_all)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&quit)
        .build()?;

    // Prefer the dedicated tray.png (simpler, better on Linux/GTK tray
    // implementations); fall back to the main window icon if missing.
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?;

    TrayIconBuilder::with_id("main-tray")
        .tooltip("Jarvis — autonomous coding agent")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_ID_TOGGLE => toggle_main_window(app),
            MENU_ID_CANCEL_ALL => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::ipc::cancel_all_running(app).await {
                        warn!(error = %e, "cancel_all_running failed");
                    }
                });
            }
            MENU_ID_QUIT => {
                info!("quit requested from tray");
                app.exit(0);
            }
            other => warn!(menu_id = other, "unhandled tray menu event"),
        })
        .build(app)?;

    Ok(())
}

fn toggle_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = app.get_webview_window("main") else {
        warn!("toggle: no `main` webview window");
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    let result = if visible {
        win.hide()
    } else {
        win.show().and_then(|_| win.set_focus())
    };
    if let Err(e) = result {
        warn!(error = %e, "failed to toggle main window visibility");
    }
}
