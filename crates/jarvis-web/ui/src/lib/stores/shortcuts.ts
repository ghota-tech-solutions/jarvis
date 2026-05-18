// Global keyboard shortcuts.
//
//   /     → focus the dashboard search input (if any)
//   g d   → /
//   g t   → /  (alias)
//   ?     → toggle help overlay
//   t     → toggle theme dark/light
//
// Shortcuts ignore keystrokes when an input or textarea has focus.

import { createSignal, onCleanup, onMount } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { toggleTheme } from './theme';

const [helpOpen, setHelpOpen] = createSignal(false);

export const useHelpOpen = () => helpOpen();
export const setHelpOpenSig = setHelpOpen;

export function useGlobalShortcuts() {
  const nav = useNavigate();
  let lastG = 0;

  const onKey = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement | null;
    const tag = target?.tagName;
    const inField = tag === 'INPUT' || tag === 'TEXTAREA' || target?.isContentEditable === true;

    if (inField) {
      // Allow Esc to clear the help even from inside fields.
      if (e.key === 'Escape' && helpOpen()) setHelpOpen(false);
      return;
    }

    if (e.key === '?') {
      setHelpOpen((v) => !v);
      e.preventDefault();
      return;
    }
    if (e.key === 'Escape' && helpOpen()) {
      setHelpOpen(false);
      return;
    }
    if (e.key === '/') {
      const input = document.querySelector<HTMLInputElement>('input[data-search]');
      input?.focus();
      e.preventDefault();
      return;
    }
    if (e.key === 't') {
      toggleTheme();
      return;
    }
    if (e.key === 'g') {
      lastG = Date.now();
      return;
    }
    if ((e.key === 'd' || e.key === 't') && Date.now() - lastG < 700) {
      nav('/');
      lastG = 0;
      return;
    }
  };

  onMount(() => {
    window.addEventListener('keydown', onKey);
    onCleanup(() => window.removeEventListener('keydown', onKey));
  });
}
