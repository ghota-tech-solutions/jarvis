// Centralized config + token bootstrap.
//
// On first load: if the URL hash contains `#token=XXX`, store it in
// sessionStorage and strip the hash so the token never leaks into the
// browser history or a copy-pasted URL.

const HASH_RE = /[#&]token=([0-9a-f]{32,128})/i;
const STORAGE_KEY = 'jarvis-token';

function bootstrapToken(): string | null {
  const m = window.location.hash.match(HASH_RE);
  if (m) {
    sessionStorage.setItem(STORAGE_KEY, m[1]);
    const cleaned = window.location.hash.replace(HASH_RE, '').replace(/^#&?/, '');
    history.replaceState(
      null,
      '',
      window.location.pathname + window.location.search + (cleaned ? '#' + cleaned : '')
    );
  }
  return sessionStorage.getItem(STORAGE_KEY);
}

// In dev, the Vite proxy forwards /jarvis.v1.Jarvis/* to :7777 — keeping
// requests same-origin so we don't pay CORS. In production builds (Tauri
// or the jarvis-web static serve), the SPA calls :7777 directly; CORS is
// allowed on the tonic-web layer for the SPA's origin.
export const API_BASE: string =
  (import.meta.env.VITE_API_BASE as string | undefined) ??
  (import.meta.env.DEV ? window.location.origin : 'http://127.0.0.1:7777');

export const SPA_BASE: string =
  (import.meta.env.VITE_SPA_BASE as string | undefined) ?? 'http://127.0.0.1:7879';

export const TOKEN: string | null = bootstrapToken();

export function getToken(): string | null {
  return sessionStorage.getItem(STORAGE_KEY);
}

export function setToken(token: string): void {
  sessionStorage.setItem(STORAGE_KEY, token);
}

export function clearToken(): void {
  sessionStorage.removeItem(STORAGE_KEY);
}
