// Applies user appearance preferences (theme / accent / density) onto the
// <html> element so CSS variables and `[data-*]` selectors react.
//
// Theme resolution:
//   - 'light' / 'dark'  → applied directly
//   - 'system'          → mirrors `prefers-color-scheme`
//
// We also write the resolved mode back into the legacy `themeAtom`
// (`jarvis-theme`) so the existing dark/light toggle button + `t` shortcut
// keep working without a refactor. When the user uses those, we flip the
// new `$theme` out of `system` into the explicit value they picked.

import { useStore } from '@nanostores/solid';
import { createEffect, onCleanup, onMount } from 'solid-js';
import { themeAtom, type ThemeName } from './stores/theme';
import { $theme, $accent, $density } from './settings';

const MQ = '(prefers-color-scheme: dark)';

function systemPref(): ThemeName {
  if (typeof window === 'undefined' || !window.matchMedia) return 'dark';
  return window.matchMedia(MQ).matches ? 'dark' : 'light';
}

function resolveTheme(mode: ReturnType<typeof $theme.get>): ThemeName {
  return mode === 'system' ? systemPref() : mode;
}

/**
 * Call once from App. Wires up DOM mutations + media-query listener.
 * Safe to invoke multiple times — listeners are cleaned up on dispose.
 */
export function useAppearance(): void {
  const themeMode = useStore($theme);
  const accent = useStore($accent);
  const density = useStore($density);

  // Theme: respond both to store changes and to OS-level preference changes
  // (so users in `system` mode get live updates).
  createEffect(() => {
    const mode = themeMode();
    const resolved = resolveTheme(mode);
    document.documentElement.setAttribute('data-theme', resolved);
    // Keep the legacy atom in sync so the header toggle stays in lockstep.
    if (themeAtom.get() !== resolved) themeAtom.set(resolved);
  });

  onMount(() => {
    if (typeof window === 'undefined' || !window.matchMedia) return;
    const mq = window.matchMedia(MQ);
    const handler = () => {
      if ($theme.get() !== 'system') return;
      const resolved = systemPref();
      document.documentElement.setAttribute('data-theme', resolved);
      if (themeAtom.get() !== resolved) themeAtom.set(resolved);
    };
    // Modern + Safari fallback.
    if (typeof mq.addEventListener === 'function') {
      mq.addEventListener('change', handler);
      onCleanup(() => mq.removeEventListener('change', handler));
    } else if (typeof (mq as MediaQueryList & { addListener?: (cb: () => void) => void }).addListener === 'function') {
      (mq as MediaQueryList & { addListener: (cb: () => void) => void }).addListener(handler);
      onCleanup(() => (mq as MediaQueryList & { removeListener: (cb: () => void) => void }).removeListener(handler));
    }
  });

  // Accent: writes `--accent` directly on <html>, overriding the theme
  // default. CSS reads `var(--accent)` everywhere already.
  createEffect(() => {
    document.documentElement.style.setProperty('--accent', accent());
  });

  // Density: posed as a data attribute. CSS rules under
  // `[data-density="compact"]` can tighten paddings — follow-up ticket.
  createEffect(() => {
    document.documentElement.dataset.density = density();
  });
}
