// Wrappers around the server-streaming RPCs that turn an async iterable
// into a Solid signal. The signal value is the latest snapshot; if the
// stream drops we expose the error and automatically retry after a delay.

import { createSignal, onCleanup, onMount, type Accessor } from 'solid-js';
import type { ConnectError } from '@connectrpc/connect';
import { jarvis } from './client';
import type { FleetUpdate } from './gen/jarvis_pb';

export interface StreamState<T> {
  value: Accessor<T | null>;
  error: Accessor<ConnectError | Error | null>;
  connected: Accessor<boolean>;
}

const RETRY_MS = 3000;

export function useFleetStream(): StreamState<FleetUpdate> {
  const [value, setValue] = createSignal<FleetUpdate | null>(null);
  const [error, setError] = createSignal<ConnectError | Error | null>(null);
  const [connected, setConnected] = createSignal(false);
  let cancelled = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const run = async () => {
    while (!cancelled) {
      try {
        setError(null);
        setConnected(true);
        const it = jarvis.streamFleet({});
        for await (const snap of it) {
          if (cancelled) return;
          setValue(snap);
        }
        // Stream ended cleanly — reconnect after a tick
        setConnected(false);
      } catch (e) {
        if (cancelled) return;
        setConnected(false);
        setError(e as ConnectError);
      }
      if (cancelled) return;
      await new Promise<void>((r) => {
        retryTimer = setTimeout(r, RETRY_MS);
      });
    }
  };

  onMount(() => {
    run();
  });

  onCleanup(() => {
    cancelled = true;
    if (retryTimer) clearTimeout(retryTimer);
  });

  return { value, error, connected };
}
