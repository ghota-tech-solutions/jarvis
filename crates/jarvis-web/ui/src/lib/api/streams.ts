// Wrappers around the server-streaming RPCs that turn an async iterable
// into a Solid signal. The signal value is the latest snapshot; if the
// stream drops we expose the error and automatically retry after a delay.

import { createEffect, createSignal, on, onCleanup, onMount, type Accessor } from 'solid-js';
import type { ConnectError } from '@connectrpc/connect';
import { jarvis } from './client';
import type {
  Event,
  FleetEdge,
  FleetFrame,
  FleetNode,
  TimelineEvent,
  TimelineSpan,
} from './gen/jarvis_pb';

// Shape consumed by the FleetDag layout — same fields as the old
// `FleetUpdate` envelope, but assembled client-side from the
// snapshot + delta stream introduced in M7.1.
export interface FleetView {
  nodes: FleetNode[];
  edges: FleetEdge[];
  tsMicros: bigint;
}

export interface StreamState<T> {
  value: Accessor<T | null>;
  error: Accessor<ConnectError | Error | null>;
  connected: Accessor<boolean>;
}

const RETRY_MS = 3000;

// M7.1: server pushes either a full `FleetSnapshot` (first frame on
// every fresh subscription) or a `FleetDelta` describing what changed
// since the previous tick. We assemble the rolling view here so the UI
// keeps consuming a single `FleetView` accessor.
export function useFleetStream(): StreamState<FleetView> {
  const [value, setValue] = createSignal<FleetView | null>(null);
  const [error, setError] = createSignal<ConnectError | Error | null>(null);
  const [connected, setConnected] = createSignal(false);
  let cancelled = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const applyFrame = (frame: FleetFrame) => {
    const kind = frame.kind;
    if (!kind) return;
    if (kind.case === 'snapshot') {
      setValue({
        nodes: [...kind.value.nodes],
        edges: [...kind.value.edges],
        tsMicros: frame.tsMicros,
      });
      return;
    }
    if (kind.case === 'delta') {
      const cur = value();
      if (!cur) {
        // Defensive: server contract says the first frame is always a
        // snapshot. If we somehow see a delta first, ignore it and wait
        // for the snapshot to arrive on reconnect.
        return;
      }
      const d = kind.value;
      // Index existing nodes for O(1) update / remove.
      const nodeMap = new Map<string, FleetNode>();
      for (const n of cur.nodes) nodeMap.set(n.taskId, n);
      for (const n of d.addedNodes) nodeMap.set(n.taskId, n);
      for (const n of d.updatedNodes) nodeMap.set(n.taskId, n);
      for (const id of d.removedNodeIds) nodeMap.delete(id);

      // Edges are de-duped by (parent, child).
      const edgeKey = (e: FleetEdge) => `${e.parentTaskId}\x00${e.childTaskId}`;
      const edgeMap = new Map<string, FleetEdge>();
      for (const e of cur.edges) edgeMap.set(edgeKey(e), e);
      for (const e of d.addedEdges) edgeMap.set(edgeKey(e), e);
      for (const e of d.removedEdges) edgeMap.delete(edgeKey(e));

      setValue({
        nodes: Array.from(nodeMap.values()),
        edges: Array.from(edgeMap.values()),
        tsMicros: frame.tsMicros,
      });
    }
  };

  const run = async () => {
    while (!cancelled) {
      try {
        setError(null);
        setConnected(true);
        // Reset on every reconnect — the server replays a full snapshot
        // as its first frame, so anything we had is stale.
        setValue(null);
        const it = jarvis.streamFleet({});
        for await (const frame of it) {
          if (cancelled) return;
          applyFrame(frame);
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

  // Mutable session state. Each navigation to a new task id starts a
  // fresh session and the previous one bails out via `cancelled`.
  type Session = {
    cancelled: boolean;
    retryTimer: ReturnType<typeof setTimeout> | null;
    lastEventId: bigint;
    id: string;
  };
  let session: Session | null = null;

  const appendEvent = (sess: Session, te: TimelineEvent) => {
    if (sess.cancelled || sess !== session) return;
    // Guard against out-of-order or duplicate ids during reconnect.
    if (te.id <= sess.lastEventId) return;
    sess.lastEventId = te.id;
    setEvents((prev) => [...prev, te]);
    if (te.tsMicros > maxTsMicros()) setMaxTsMicros(te.tsMicros);
    if (minTsMicros() === 0n) setMinTsMicros(te.tsMicros);
  };

  const run = async (sess: Session) => {
    if (!sess.id) return;

    // 1) Initial snapshot — backfills events + spans + bounds.
    //    `includeAncestors: true` so a follow-up task page shows the full
    //    conversation chain, not just the latest user turn.
    try {
      const snap = await jarvis.getTimeline({ id: sess.id, includeAncestors: true });
      if (sess.cancelled || sess !== session) return;
      setEvents(snap.events);
      setSpans(snap.spans);
      setMinTsMicros(snap.minTsMicros);
      setMaxTsMicros(snap.maxTsMicros);
      sess.lastEventId = snap.events.reduce(
        (acc, e) => (e.id > acc ? e.id : acc),
        0n,
      );
      setLoaded(true);
    } catch (e) {
      if (sess.cancelled || sess !== session) return;
      setError(e as ConnectError);
    }

    // 2) Tail the stream from where the snapshot stopped. The loop ends
    //    when the session is replaced (navigation) or the component
    //    unmounts.
    while (!sess.cancelled && sess === session) {
      try {
        setError(null);
        setConnected(true);
        const it = jarvis.streamEvents({
          taskId: sess.id,
          follow: true,
          sinceId: sess.lastEventId,
          includeAncestors: true,
        });
        for await (const ev of it) {
          if (sess.cancelled || sess !== session) return;
          appendEvent(sess, eventToTimelineEvent(ev));
        }
        setConnected(false);
      } catch (e) {
        if (sess.cancelled || sess !== session) return;
        setConnected(false);
        setError(e as ConnectError);
      }
      if (sess.cancelled || sess !== session) return;
      await new Promise<void>((r) => {
        sess.retryTimer = setTimeout(r, RETRY_MS);
      });
    }
  };

  const startSession = (id: string) => {
    // Cancel the previous session, if any.
    if (session) {
      session.cancelled = true;
      if (session.retryTimer) clearTimeout(session.retryTimer);
    }
    // Reset visible state so the user doesn't see stale events from the
    // previous task during the snapshot fetch.
    setEvents([]);
    setSpans([]);
    setMinTsMicros(0n);
    setMaxTsMicros(0n);
    setLoaded(false);
    setConnected(false);
    setError(null);
    const sess: Session = { cancelled: false, retryTimer: null, lastEventId: 0n, id };
    session = sess;
    run(sess);
  };

  onMount(() => {
    startSession(taskIdAccessor());
  });

  // React to taskId changes (e.g. navigating to a follow-up task via
  // the Ask-for-follow-up form, or clicking a child task in the DAG).
  // Without this, the hook would only ever stream the task that was
  // mounted first.
  createEffect(on(taskIdAccessor, (id, prev) => {
    if (prev === undefined) return; // initial run handled by onMount
    if (id === prev) return;
    startSession(id);
  }));

  onCleanup(() => {
    if (session) {
      session.cancelled = true;
      if (session.retryTimer) clearTimeout(session.retryTimer);
      session = null;
    }
  });

  return { events, spans, minTsMicros, maxTsMicros, loaded, connected, error };
}
