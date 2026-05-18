// Theme preference — persisted to localStorage via @nanostores/persistent.
//
// Two themes (dark/light) mirror the TUI palette. The CSS applies based on
// the `data-theme` attribute on <html>; this store updates it on change.

import { persistentAtom } from '@nanostores/persistent';
import { useStore } from '@nanostores/solid';
import { createEffect } from 'solid-js';

export type ThemeName = 'dark' | 'light';

export const themeAtom = persistentAtom<ThemeName>('jarvis-theme', 'dark');

export function useTheme() {
  return useStore(themeAtom);
}

export function setTheme(t: ThemeName) {
  themeAtom.set(t);
}

export function toggleTheme() {
  themeAtom.set(themeAtom.get() === 'dark' ? 'light' : 'dark');
}

/** Apply the theme attribute on <html> whenever the store changes. */
export function bindThemeToDom() {
  const t = useTheme();
  createEffect(() => {
    document.documentElement.setAttribute('data-theme', t());
  });
}
