// § F1.6 — i18n entry point.
//
// Exposes:
//   - `$locale` (persistent atom, "en" | "fr", defaults to "en" — auto-
//     detection from `navigator.language` happens only once at first
//     boot when the atom has no stored value).
//   - `useT()` — a SolidJS hook returning a reactive translator. Call
//     sites use `t().nav.dashboard` for typed dot-access (the dict is
//     read directly, so VSCode autocompletes the path).
//   - `translate(path, vars?)` — string-keyed alternative for dynamic
//     paths (rare; prefer the typed accessor).
//
// Adopting `@solid-primitives/i18n` would give us advanced features
// (template literals, plural forms, async loading), but the dictionaries
// here are tiny and synchronous — a thin custom wrapper keeps the bundle
// smaller and lets call sites stay type-safe end-to-end.

import { createMemo } from 'solid-js';
import { useStore } from '@nanostores/solid';
import { persistentAtom } from '@nanostores/persistent';
import { DICTS, DEFAULT_LOCALE, type Dict, type Locale } from './dict';

export type { Locale } from './dict';
export { SUPPORTED_LOCALES, DEFAULT_LOCALE } from './dict';

function detectInitialLocale(): Locale {
  if (typeof navigator === 'undefined') return DEFAULT_LOCALE;
  const lang = (navigator.language || '').toLowerCase();
  if (lang.startsWith('fr')) return 'fr';
  return DEFAULT_LOCALE;
}

export const $locale = persistentAtom<Locale>(
  'jarvis:locale',
  detectInitialLocale(),
);

/** Reactive translator. Returns the dict for the active locale, falling
 *  back to English when the active locale is missing (defensive — the
 *  type system already enforces parity). */
export function useT() {
  const loc = useStore($locale);
  return createMemo(() => DICTS[loc()] ?? DICTS.en);
}

/** Imperative dict accessor for non-component code (e.g. notifications).
 *  Subscribes to the locale store at call time but returns a snapshot. */
export function currentDict(): Dict {
  return DICTS[$locale.get()] ?? DICTS.en;
}
