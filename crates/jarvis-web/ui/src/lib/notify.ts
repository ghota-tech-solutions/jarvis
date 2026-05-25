// § F1.10 — notification adapter.
//
// Prefers the Tauri plugin-notification when running inside the desktop
// shell (detected via `window.__TAURI_INTERNALS__`), so notifications land
// in the OS notification center properly even when the SPA window is
// hidden to tray. Falls back to the browser Notification API on the web
// surface (or when Tauri detection fails).
//
// Permission handling is unified: `requestNotificationPermission()` asks
// once via whichever backend is active.
//
// All entry points are no-ops when the runtime doesn't support
// notifications at all (e.g. a hardened browser context).

type Payload = {
  title: string;
  body?: string;
  /** Optional dedupe key — Tauri ignores; browser uses as the `tag`. */
  tag?: string;
};

function inTauri(): boolean {
  return (
    typeof window !== 'undefined' &&
    // Tauri 2 injects this at preload time. Older codepaths checked
    // `window.__TAURI__`; the underscore-internals path is the stable
    // post-2.0 surface.
    ('__TAURI_INTERNALS__' in window || '__TAURI__' in window)
  );
}

/** Ask the user once for notification permission via the active backend.
 *  Resolves to true if granted, false otherwise. Safe to call multiple
 *  times — the underlying backend deduplicates. */
export async function requestNotificationPermission(): Promise<boolean> {
  if (inTauri()) {
    try {
      const mod = await import('@tauri-apps/plugin-notification');
      const granted = await mod.isPermissionGranted();
      if (granted) return true;
      const status = await mod.requestPermission();
      return status === 'granted';
    } catch {
      return false;
    }
  }
  if (typeof Notification === 'undefined') return false;
  if (Notification.permission === 'granted') return true;
  if (Notification.permission === 'denied') return false;
  try {
    const status = await Notification.requestPermission();
    return status === 'granted';
  } catch {
    return false;
  }
}

/** Fire a single notification through the best available backend.
 *  Silently no-ops if permission is missing or the runtime is unsupported. */
export async function notify(p: Payload): Promise<void> {
  if (inTauri()) {
    try {
      const mod = await import('@tauri-apps/plugin-notification');
      const granted = await mod.isPermissionGranted();
      if (!granted) return;
      mod.sendNotification({ title: p.title, body: p.body });
      return;
    } catch {
      // fall through to browser API
    }
  }
  if (typeof Notification === 'undefined') return;
  if (Notification.permission !== 'granted') return;
  try {
    new Notification(p.title, { body: p.body, tag: p.tag });
  } catch {
    /* ignored */
  }
}
