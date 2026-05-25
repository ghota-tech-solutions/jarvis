// Centralized user preferences — persisted via @nanostores/persistent.
//
// All atoms persist to localStorage under the `jarvis:*` namespace so they
// survive reloads and HMR. Reads go through `@nanostores/solid::useStore`
// in components; writes via `$atom.set(value)` from the Settings page or
// other call sites (toggles, command palette).
//
// The existing `jarvis-theme` (dark/light) atom in `stores/theme.ts` is
// kept untouched for back-compat with the global `t` shortcut and the
// header button. The new `$theme` here adds a `system` mode that follows
// `prefers-color-scheme` and writes through to the legacy atom so the rest
// of the app keeps working without a refactor sweep.

import { persistentAtom } from '@nanostores/persistent';

export type ThemeMode = 'light' | 'dark' | 'system';
export type Density = 'comfortable' | 'compact';
export type DefaultSandbox = 'native' | 'docker' | 'wsl2';
export type DefaultRouting = 'auto' | 'local_only' | 'remote_only';

/** 8 curated accent presets — kept identical between light and dark. */
export const ACCENT_PRESETS: ReadonlyArray<{ name: string; value: string }> = [
  { name: 'gold',    value: 'rgb(212, 180, 120)' },
  { name: 'blue',    value: 'rgb(96, 165, 250)' },
  { name: 'green',   value: 'rgb(110, 200, 120)' },
  { name: 'purple',  value: 'rgb(167, 139, 250)' },
  { name: 'pink',    value: 'rgb(244, 114, 182)' },
  { name: 'red',     value: 'rgb(248, 113, 113)' },
  { name: 'orange',  value: 'rgb(251, 146, 60)' },
  { name: 'teal',    value: 'rgb(94, 199, 199)' },
];

export const DEFAULT_ACCENT = ACCENT_PRESETS[0].value;

export const $theme = persistentAtom<ThemeMode>('jarvis:theme', 'system');
export const $accent = persistentAtom<string>('jarvis:accent', DEFAULT_ACCENT);
export const $density = persistentAtom<Density>('jarvis:density', 'comfortable');

export const $defaultSandbox = persistentAtom<DefaultSandbox>(
  'jarvis:default-sandbox',
  'native',
);
export const $defaultRouting = persistentAtom<DefaultRouting>(
  'jarvis:default-routing',
  'auto',
);
export const $defaultMaxSteps = persistentAtom<number>(
  'jarvis:default-max-steps',
  20,
  { encode: String, decode: (v) => Number(v) || 20 },
);
