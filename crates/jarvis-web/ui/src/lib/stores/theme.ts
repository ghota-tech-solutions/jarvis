// Theme preference — persisted to localStorage via @nanostores/persistent.
//
// Two themes (dark/light) mirror the TUI palette. The CSS applies based on
// the `data-theme` attribute on <html>; this store updates it on change.

import { persistentAtom } from '@nanostores/persistent';
import { useStore } from '@nanostores/solid';
import { createEffect } from 'solid-js';
import { $theme } from '~/lib/settings';

export type ThemeName = 'dark' | 'light';

export const themeAtom = persistentAtom<ThemeName>('jarvis-theme', 'dark');

export function useTheme() {
  return useStore(themeAtom);
}

export function setTheme(t: ThemeName) {
  themeAtom.set(t);
  // Also write through to the new settings atom so Settings page +
  // header button + `t` shortcut stay in lockstep. Forces user out of
  // `system` mode (an explicit toggle is an explicit choice).
  $theme.set(t);
}

export function toggleTheme() {
  const next: ThemeName = themeAtom.get() === 'dark' ? 'light' : 'dark';
  setTheme(next);
}

/**
 * @deprecated Use `useAppearance()` from `~/lib/theme-apply.ts` which also
 * handles `system` mode, accent, and density. Kept as a thin alias so
 * legacy imports keep compiling during the transition.
 */
export function bindThemeToDom() {
  const t = useTheme();
  createEffect(() => {
    document.documentElement.setAttribute('data-theme', t());
  });
}
