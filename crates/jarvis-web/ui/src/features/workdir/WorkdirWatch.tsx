// § F2.6 — Live workdir browser panel.
//
// Subscribes to the daemon's `WatchWorkdir` streaming RPC (T2.10) and
// renders a rolling log of filesystem events for the task's workdir.
// Pure observability v1 — no file content preview yet, just the event
// stream so the user can see the agent's edits land in real time.
//
// The stream auto-reconnects with a 2-second backoff on transport
// errors. The event log is capped at MAX_EVENTS so a busy workdir
// (cargo build, npm install) doesn't blow client memory.

import {
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  type Component,
} from 'solid-js';
import { jarvis } from '~/lib/api/client';
import type { FsEvent } from '~/lib/api/gen/jarvis_pb';

const MAX_EVENTS = 500;

type Entry = {
  id: number;
  path: string;
  kind: string;
  ts: bigint;
};

function shortTime(micros: bigint): string {
  if (!micros) return '';
  const d = new Date(Number(micros / 1000n));
  return d.toLocaleTimeString(undefined, { hour12: false });
}

function kindClass(kind: string): string {
  switch (kind) {
    case 'created':
      return 'good';
    case 'removed':
      return 'error';
    case 'modified':
      return 'warn';
    case 'renamed':
      return 'accent';
    default:
      return 'dim';
  }
}

const WorkdirWatch: Component<{ workdir: string }> = (p) => {
  const [events, setEvents] = createSignal<Entry[]>([]);
  const [connected, setConnected] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [paused, setPaused] = createSignal(false);
  const [filter, setFilter] = createSignal('');

  let counter = 0;
  let abort: AbortController | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  const push = (ev: FsEvent) => {
    if (paused()) return;
    counter += 1;
    setEvents((prev) => {
      const next = [
        ...prev,
        { id: counter, path: ev.path, kind: ev.kind, ts: ev.tsMicros },
      ];
      // Keep the tail so the latest events stay visible.
      return next.length > MAX_EVENTS
        ? next.slice(next.length - MAX_EVENTS)
        : next;
    });
  };

  const start = () => {
    if (!p.workdir) return;
    abort?.abort();
    abort = new AbortController();
    setError(null);
    (async () => {
      try {
        setConnected(true);
        for await (const ev of jarvis.watchWorkdir(
          { workdir: p.workdir },
          { signal: abort!.signal },
        )) {
          push(ev);
        }
        setConnected(false);
        // Stream ended cleanly — try to reconnect after a delay.
        reconnectTimer = setTimeout(start, 2000);
      } catch (e) {
        setConnected(false);
        if (abort?.signal.aborted) return; // user navigated away
        setError(String(e));
        reconnectTimer = setTimeout(start, 2000);
      }
    })();
  };

  // (Re)start whenever the workdir prop changes.
  createEffect(() => {
    const w = p.workdir;
    if (!w) return;
    start();
  });

  onCleanup(() => {
    abort?.abort();
    if (reconnectTimer) clearTimeout(reconnectTimer);
  });

  const filtered = () => {
    const f = filter().trim().toLowerCase();
    if (!f) return events();
    return events().filter((e) => e.path.toLowerCase().includes(f));
  };

  return (
    <div class="workdir-watch">
      <header class="workdir-watch-header">
        <span class={`pill ${connected() ? 'good' : 'warn'}`}>
          {connected() ? 'connected' : 'reconnecting…'}
        </span>
        <span class="dim" style="font-size: 11px">
          {events().length} event{events().length === 1 ? '' : 's'}
        </span>
        <input
          type="search"
          class="input"
          placeholder="filter path…"
          style="font-size: 11px; padding: 0.2rem 0.4rem; max-width: 200px"
          value={filter()}
          onInput={(e) => setFilter(e.currentTarget.value)}
        />
        <span style="flex: 1" />
        <button
          type="button"
          class="btn ghost"
          style="font-size: 11px; padding: 0.2rem 0.5rem"
          onClick={() => setPaused((v) => !v)}
        >
          {paused() ? '▶ Resume' : '⏸ Pause'}
        </button>
        <button
          type="button"
          class="btn ghost"
          style="font-size: 11px; padding: 0.2rem 0.5rem"
          onClick={() => setEvents([])}
        >
          Clear
        </button>
      </header>
      <Show when={error()}>
        <p class="error" style="font-size: 11px; margin: 0.3rem 0">
          {error()}
        </p>
      </Show>
      <Show
        when={filtered().length > 0}
        fallback={
          <p class="dim" style="font-size: 11px; padding: 1rem; text-align: center">
            {events().length === 0
              ? 'waiting for filesystem events…'
              : `no event matches "${filter()}"`}
          </p>
        }
      >
        <ul class="workdir-watch-log">
          <For each={filtered().slice().reverse()}>
            {(e) => (
              <li class="workdir-watch-row">
                <span class="dim workdir-watch-ts">{shortTime(e.ts)}</span>
                <span class={`pill ${kindClass(e.kind)} workdir-watch-kind`}>
                  {e.kind}
                </span>
                <code class="workdir-watch-path">{e.path}</code>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
};

export default WorkdirWatch;
