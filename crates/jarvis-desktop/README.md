# jarvis-desktop

Tauri 2 desktop wrapper for the Jarvis SolidJS SPA. Bundles:

- The static `dist/` build of `crates/jarvis-web/ui/` as the WebView contents
- A sidecar process that auto-spawns `jarvis-daemon` if it isn't already running
- IPC commands: `daemon_status`, `start_daemon`, `stop_daemon`, `read_web_token`

## Dev

```powershell
# 1. Install Tauri CLI once
cargo install tauri-cli --version "^2.0"

# 2. Build the SPA first time (Tauri will reuse `bun run dev` afterwards)
cd crates/jarvis-web/ui && bun install && cd ../../..

# 3. Run the desktop app in dev mode (auto-launches vite + daemon)
cd crates/jarvis-desktop && cargo tauri dev
```

## Build

```powershell
cd crates/jarvis-desktop && cargo tauri build
# Output: target/release/bundle/{msi,nsis,deb,dmg}/...
```

## Icons

`icons/` is intentionally empty in this scaffold. Add `icon.png`,
`32x32.png`, `128x128.png`, `icon.ico`, `icon.icns` before shipping —
Tauri's `cargo tauri icon path/to/icon.png` generates all sizes from a
single source.

## Production notes

- The daemon is currently spawned from the workspace's `target/debug` or
  `target/release` directory; for a packaged install, ship it as a Tauri
  `externalBin` so it lives alongside the GUI executable.
- `read_web_token` looks up `<data_dir>/web.token` in three candidate
  locations; in production it should be standardised on
  `app.path().app_data_dir().join("web.token")`.
