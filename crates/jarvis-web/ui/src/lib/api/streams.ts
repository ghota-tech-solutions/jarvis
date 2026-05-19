// Wrappers around the server-streaming RPCs that turn an async iterable
// into a Solid signal. The signal value is the latest snapshot; if the
// stream drops we expose the error and automatically retry after a delay.

import { createSignal, onCleanup, onMount, type Accessor } from 'solid-js';
import type { ConnectError } from '@connectrpc/connect';
import { jarvis } from './client';
import type { Event, FleetUpdate, TimelineEvent, TimelineSpan } from './gen/jarvis_pb';

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

// § C.M-D — live task event stream.
//
// Replaces the 3-second polling of getTimeline with: (1) a one-shot
// snapshot to backfill events + pre-computed spans + min/max bounds,
// then (2) a server-streaming subscription that pushes each new Event
// into the local store as soon as the ledger appends it.
//
// The Event message produced by streamEvents has the same wire shape as
// TimelineEvent (id, ts_micros, task_id, agent_id, kind, subject,
// payload_json, parent_evt) so we widen its type at the boundary and
// reuse the existing renderers unchanged.
//
// Reconnect strategy: same backoff as useFleetStream (3 s). On reconnect
// we pass `sinceId: lastEventId` so the daemon only emits events the
// client hasn't seen yet — no double-render, no full refetch.
//
// `includeAncestors: true` so follow-up chains (M9 / the parent-context
// fix) get the prior turns backfilled into the initial stream window.

export interface TaskStreamState {
  events: Accessor<TimelineEvent[]>;
  spans: Accessor<TimelineSpan[]>;
  minTsMicros: Accessor<bigint>;
  maxTsMicros: Accessor<bigint>;
  loaded: Accessor<boolean>;
  connected: Accessor<boolean>;
  error: Accessor<ConnectError | Error | null>;
}

const eventToTimelineEvent = (e: Event): TimelineEvent =>
  // The two messages share field names + types; only the `$typeName`
  // differs. Casting through unknown is safe here because we don't rely
  // on the brand at runtime — the SPA renderers read by field.
  ({
    ...e,
    $typeName: 'jarvis.v1.TimelineEvent',
  } as unknown as TimelineEvent);

export function useTaskEventStream(taskIdAccessor: Accessor<string>): TaskStreamState {
  const [events, setEvents] = createSignal<TimelineEvent[]>([]);
  const [spans, setSpans] = createSignal<TimelineSpan[]>([]);
  const [minTsMicros, setMinTsMicros] = createSignal<bigint>(0n);
  const [maxTsMicros, setMaxTsMicros] = createSignal<bigint>(0n);
  const [loaded, setLoaded] = createSignal(false);
  const [connected, setConnected] = createSignal(false);
  const [error, setError] = createSignal<ConnectError | Error | null>(null);

  let cancelled = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let lastEventId = 0n;

  const appendEvent = (te: TimelineEvent) => {
    // Guard against out-of-order or duplicate ids during reconnect.
    if (te.id <= lastEventId) return;
    lastEventId = te.id;
    setEvents((prev) => [...prev, te]);
    if (te.tsMicros > maxTsMicros()) setMaxTsMicros(te.tsMicros);
    if (minTsMicros() === 0n) setMinTsMicros(te.tsMicros);
  };

  const run = async () => {
    const id = taskIdAccessor();
    if (!id) return;

    // 1) Initial snapshot — backfills events + spans + bounds.
    try {
      const snap = await jarvis.getTimeline({ id });
      if (cancelled) return;
      setEvents(snap.events);
      setSpans(snap.spans);
      setMinTsMicros(snap.minTsMicros);
      setMaxTsMicros(snap.maxTsMicros);
      lastEventId = snap.events.reduce(
        (acc, e) => (e.id > acc ? e.id : acc),
        0n,
      );
      setLoaded(true);
    } catch (e) {
      if (cancelled) return;
      setError(e as ConnectError);
    }

    // 2) Tail the stream from where the snapshot stopped.
    while (!cancelled) {
      try {
        setError(null);
        setConnected(true);
        const it = jarvis.streamEvents({
          taskId: id,
          follow: true,
          sinceId: lastEventId,
          includeAncestors: true,
        });
        for await (const ev of it) {
          if (cancelled) return;
          appendEvent(eventToTimelineEvent(ev));
        }
        // Stream ended cleanly → reconnect after backoff.
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

  return { events, spans, minTsMicros, maxTsMicros, loaded, connected, error };
}
