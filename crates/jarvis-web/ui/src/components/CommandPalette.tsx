// Command palette — Cmd/Ctrl+K toggles, fuzzy match, Enter runs.
//
// Each command has an id, a label, a hint, and a `run` callback. Some are
// pure navigation (router push); some prompt for an argument inline before
// firing the actual RPC. No external fuzzy lib — the catalog is small
// enough (~10 entries) that a simple substring-then-acronym scorer beats
// the import cost.

import {
  Show,
  createMemo,
  createSignal,
  createEffect,
  onCleanup,
  onMount,
  For,
  type Component,
} from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { useQueryClient } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';
import { qkTaskList } from '~/lib/api/queries';
import { toggleTheme } from '~/lib/stores/theme';
import { setHelpOpenSig } from '~/lib/stores/shortcuts';

type Command = {
  id: string;
  label: string;
  hint?: string;
  /** When set, the palette switches to "arg entry" mode after pick. */
  argLabel?: string;
  run: (arg: string) => Promise<void> | void;
};

const [open, setOpen] = createSignal(false);
export const openPalette = () => setOpen(true);
export const closePalette = () => setOpen(false);

const score = (q: string, label: string, id: string): number => {
  if (!q) return 1;
  const ql = q.toLowerCase();
  const ll = label.toLowerCase();
  if (ll.includes(ql)) return 100 - ll.indexOf(ql);
  if (id.includes(ql)) return 50;
  // Acronym match: "gd" → "go dashboard"
  const initials = label
    .split(/\s+/)
    .map((w) => w[0]?.toLowerCase() ?? '')
    .join('');
  if (initials.startsWith(ql)) return 30;
  return 0;
};

const CommandPalette: Component = () => {
  const nav = useNavigate();
  const qc = useQueryClient();
  const [query, setQuery] = createSignal('');
  const [selected, setSelected] = createSignal(0);
  const [pendingCmd, setPendingCmd] = createSignal<Command | null>(null);
  const [argValue, setArgValue] = createSignal('');

  let inputRef!: HTMLInputElement;

  const commands: Command[] = [
    {
      id: 'goto-dashboard',
      label: 'Go to Dashboard',
      hint: 'g d',
      run: () => nav('/'),
    },
    {
      id: 'goto-fleet',
      label: 'Go to Fleet DAG',
      run: () => nav('/fleet'),
    },
    {
      id: 'goto-memory',
      label: 'Go to Memory',
      run: () => nav('/memory'),
    },
    {
      id: 'toggle-theme',
      label: 'Toggle theme dark/light',
      hint: 't',
      run: () => toggleTheme(),
    },
    {
      id: 'show-help',
      label: 'Show keyboard shortcuts',
      hint: '?',
      run: () => setHelpOpenSig(true),
    },
    {
      id: 'new-task',
      label: 'New task',
      argLabel: 'Describe the goal',
      run: async (goal) => {
        const handle = await jarvis.submitTask({
          goal,
          workdir: '',
          sandbox: '',
          netPolicy: '',
          routingPolicy: '',
          useWorktree: false,
          maxSteps: 0,
          baseRef: '',
          requireCaps: [],
          parentTaskId: '',
          resumeFrom: '',
        });
        await qc.invalidateQueries({ queryKey: qkTaskList(true) });
        nav(`/task/${handle.id}`);
      },
    },
    {
      id: 'quick-ask',
      label: 'Quick Ask (streaming)',
      argLabel: 'Your question',
      run: async (prompt) => {
        // Quick Ask requires a streaming UI surface — easiest is to route
        // to Dashboard with the prompt prefilled. For v1 we just nav home;
        // the user can paste manually. Later: lift Quick Ask into a
        // dedicated route + URL param.
        nav('/');
        // Store the prompt in sessionStorage so Dashboard can pick it up.
        sessionStorage.setItem('jarvis-pending-ask', prompt);
      },
    },
    {
      id: 'spawn-explorer',
      label: 'Spawn explorer sub-agent',
      argLabel: 'What should the explorer look into?',
      run: async (goal) => {
        const handle = await jarvis.submitTask({
          goal: `[explorer role] ${goal}`,
          workdir: '',
          sandbox: '',
          netPolicy: '',
          routingPolicy: '',
          useWorktree: false,
          maxSteps: 0,
          baseRef: '',
          requireCaps: [],
          parentTaskId: '',
          resumeFrom: '',
        });
        await qc.invalidateQueries({ queryKey: qkTaskList(true) });
        nav(`/task/${handle.id}`);
      },
    },
  ];

  const filtered = createMemo(() => {
    const q = query();
    const ranked = commands
      .map((c) => ({ c, s: score(q, c.label, c.id) }))
      .filter((x) => x.s > 0)
      .sort((a, b) => b.s - a.s);
    return ranked.map((x) => x.c);
  });

  // Reset selection when the filter changes.
  createEffect(() => {
    filtered();
    setSelected(0);
  });

  // Focus the input when the palette opens.
  createEffect(() => {
    if (open()) {
      queueMicrotask(() => inputRef?.focus());
    } else {
      setQuery('');
      setPendingCmd(null);
      setArgValue('');
    }
  });

  // Global keybind: Cmd/Ctrl + K toggles.
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setOpen((v) => !v);
      }
    };
    window.addEventListener('keydown', onKey);
    onCleanup(() => window.removeEventListener('keydown', onKey));
  });

  const onPaletteKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      setOpen(false);
      return;
    }
    if (pendingCmd()) {
      if (e.key === 'Enter') {
        e.preventDefault();
        const cmd = pendingCmd()!;
        const arg = argValue();
        if (!arg.trim()) return;
        void Promise.resolve(cmd.run(arg)).finally(() => setOpen(false));
      }
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      const max = Math.max(0, filtered().length - 1);
      setSelected((s) => Math.min(s + 1, max));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setSelected((s) => Math.max(s - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const cmd = filtered()[selected()];
      if (!cmd) return;
      if (cmd.argLabel) {
        setPendingCmd(cmd);
      } else {
        void Promise.resolve(cmd.run('')).finally(() => setOpen(false));
      }
    }
  };

  return (
    <Show when={open()}>
      <div class="cmdp-backdrop" onClick={() => setOpen(false)}>
        <div class="cmdp-modal" onClick={(e) => e.stopPropagation()}>
          <Show
            when={pendingCmd()}
            fallback={
              <>
                <input
                  ref={inputRef}
                  class="cmdp-input"
                  type="text"
                  placeholder="Type a command — Cmd+K to close"
                  value={query()}
                  onInput={(e) => setQuery(e.currentTarget.value)}
                  onKeyDown={onPaletteKey}
                />
                <ul class="cmdp-list">
                  <For each={filtered()}>
                    {(cmd, i) => (
                      <li
                        class={`cmdp-item ${i() === selected() ? 'cmdp-active' : ''}`}
                        onMouseEnter={() => setSelected(i())}
                        onClick={() => {
                          if (cmd.argLabel) setPendingCmd(cmd);
                          else {
                            void Promise.resolve(cmd.run('')).finally(() =>
                              setOpen(false),
                            );
                          }
                        }}
                      >
                        <span>{cmd.label}</span>
                        <Show when={cmd.hint}>
                          <kbd>{cmd.hint}</kbd>
                        </Show>
                      </li>
                    )}
                  </For>
                  <Show when={filtered().length === 0}>
                    <li class="cmdp-empty">no match</li>
                  </Show>
                </ul>
              </>
            }
          >
            {(cmd) => (
              <>
                <header class="cmdp-arg-header">
                  <span class="dim">{cmd().label} →</span>
                  <span class="accent">{cmd().argLabel}</span>
                </header>
                <input
                  ref={inputRef}
                  class="cmdp-input"
                  type="text"
                  placeholder="type then Enter — Esc cancels"
                  value={argValue()}
                  onInput={(e) => setArgValue(e.currentTarget.value)}
                  onKeyDown={onPaletteKey}
                />
              </>
            )}
          </Show>
        </div>
      </div>
    </Show>
  );
};

export default CommandPalette;
