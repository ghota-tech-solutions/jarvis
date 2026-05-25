// Scrubbable reasoning timeline.
//
// Renders events + pre-computed spans on a HiDPI canvas.
// Interaction:
//   - drag the playhead (the triangular handle in the header)
//   - click a span / point event → select it; the parent gets a callback
//   - mouse wheel → zoom (with Ctrl/Cmd) or pan (without)
//   - keyboard: [/] prev/next span, j/k prev/next event, 1..4 toggle lanes,
//               'f' fit, 'l' toggle follow-live, Esc clear selection
//
// Performance budget: <16ms per redraw at 10k events on a 1920x300 viewport.
// We hit it by binary-searching the visible time window before drawing —
// most events are off-screen and skipped.

import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  type Component,
} from 'solid-js';
import type { TimelineEvent, TimelineSpan } from '~/lib/api/gen/jarvis_pb';
import {
  defaultState,
  defaultTheme,
  type Lane,
  type TimelineState,
} from './types';
import { computePhaseSegments, pxToUs, render } from './canvas-renderer';
import { hitTest, isOnPlayhead, naturalHeight } from './hit-test';

type Props = {
  events: TimelineEvent[];
  spans: TimelineSpan[];
  minTs: bigint;
  maxTs: bigint;
  onSelect?: (evtId: number) => void;
  selectedEvtId?: number;
};

const Timeline: Component<Props> = (p) => {
  let canvas!: HTMLCanvasElement;
  let container!: HTMLDivElement;

  const [state, setState] = createSignal<TimelineState>(defaultState());
  const [hoverEvtId, setHoverEvtId] = createSignal(0);
  const [width, setWidth] = createSignal(800);
  const [dragging, setDragging] = createSignal<'playhead' | 'pan' | null>(null);
  // M12.S3: replay mode — animates the playhead from minTs to maxTs at the
  // chosen wall-clock speed. `null` = stopped, otherwise the multiplier
  // (1 = realtime, 2 = 2x faster, 4 = 4x).
  const [playSpeed, setPlaySpeed] = createSignal<number | null>(null);

  // Fit the timeline to the data when it first arrives, OR when followLive
  // is on and new max_ts pushes the right edge past the viewport.
  createEffect(() => {
    const minTs = Number(p.minTs);
    const maxTs = Number(p.maxTs);
    if (minTs === 0 || maxTs === 0) return;
    const w = width();
    setState((s) => {
      // First-time fit
      if (s.panUs === 0 && s.usPerPx === 1000) {
        const total = Math.max(maxTs - minTs, 1_000_000); // at least 1s
        const usPerPx = (total * 1.05) / Math.max(1, w - 60);
        return {
          ...s,
          panUs: minTs - Math.round(total * 0.025),
          usPerPx,
          playheadUs: maxTs,
        };
      }
      if (s.followLive) {
        return { ...s, playheadUs: maxTs };
      }
      return s;
    });
  });

  // ResizeObserver keeps canvas dimensions in sync with the container.
  onMount(() => {
    const ro = new ResizeObserver((entries) => {
      const r = entries[0].contentRect;
      setWidth(r.width);
    });
    ro.observe(container);
    onCleanup(() => ro.disconnect());

    // Keyboard shortcuts (global while the canvas is in DOM)
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA') return;
      switch (e.key) {
        case '[':
          jumpSpan(-1);
          break;
        case ']':
          jumpSpan(+1);
          break;
        case 'j':
          jumpEvent(+1);
          break;
        case 'k':
          jumpEvent(-1);
          break;
        case '1': toggleLane('tools'); break;
        case '2': toggleLane('llm'); break;
        case '3': toggleLane('plan'); break;
        case '4': toggleLane('verdict'); break;
        case 'f': fit(); break;
        case 'l':
          setState((s) => ({ ...s, followLive: !s.followLive }));
          break;
        case 'Escape':
          setState((s) => ({ ...s, selectedEvtId: 0 }));
          p.onSelect?.(0);
          break;
      }
    };
    window.addEventListener('keydown', onKey);
    onCleanup(() => window.removeEventListener('keydown', onKey));
  });

  const theme = defaultTheme();

  // § F2.1 — phase segments, recomputed when events / maxTs change.
  // Empty array → ribbon hidden, lanes flush against header (zero-cost
  // when the task has no phase metadata).
  const phaseSegments = createMemo(() =>
    computePhaseSegments(p.events, p.maxTs),
  );
  const hasPhases = createMemo(() => phaseSegments().length > 0);

  const heightCss = createMemo(() => naturalHeight(state(), hasPhases()));

  // Redraw whenever state, events, spans, or size change.
  createEffect(() => {
    const w = width();
    const h = heightCss();
    if (!canvas || w === 0 || h === 0) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(w * dpr);
    canvas.height = Math.floor(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    const sel = p.selectedEvtId ?? state().selectedEvtId;
    render({
      ctx,
      widthCss: w,
      heightCss: h,
      dpr,
      events: p.events,
      spans: p.spans,
      minTs: p.minTs,
      maxTs: p.maxTs,
      state: { ...state(), selectedEvtId: sel },
      theme,
      hoverEvtId: hoverEvtId(),
      phaseSegments: phaseSegments(),
    });
  });

  const localPos = (e: PointerEvent | WheelEvent | MouseEvent): [number, number] => {
    const r = canvas.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  };

  const onPointerDown = (e: PointerEvent) => {
    const [x, y] = localPos(e);
    if (isOnPlayhead(x, y, state())) {
      setDragging('playhead');
      canvas.setPointerCapture(e.pointerId);
      return;
    }
    const id = hitTest(x, y, p.events, p.spans, state(), hasPhases());
    if (id !== 0) {
      setState((s) => ({ ...s, selectedEvtId: id }));
      p.onSelect?.(id);
    } else {
      setDragging('pan');
      canvas.setPointerCapture(e.pointerId);
    }
  };

  let lastPanX = 0;
  const onPointerMove = (e: PointerEvent) => {
    const [x, y] = localPos(e);
    if (dragging() === 'playhead') {
      const us = pxToUs(x, state());
      setState((s) => ({ ...s, playheadUs: us, followLive: false }));
      return;
    }
    if (dragging() === 'pan') {
      const dx = e.clientX - lastPanX;
      lastPanX = e.clientX;
      setState((s) => ({ ...s, panUs: s.panUs - dx * s.usPerPx, followLive: false }));
      return;
    }
    const id = hitTest(x, y, p.events, p.spans, state(), hasPhases());
    setHoverEvtId(id);
    canvas.style.cursor = id !== 0 || isOnPlayhead(x, y, state()) ? 'pointer' : 'default';
  };

  const onPointerDownStart = (e: PointerEvent) => {
    lastPanX = e.clientX;
    onPointerDown(e);
  };

  const onPointerUp = (e: PointerEvent) => {
    if (dragging()) {
      canvas.releasePointerCapture(e.pointerId);
      setDragging(null);
    }
  };

  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      const [x] = localPos(e);
      const usAtCursor = pxToUs(x, state());
      const factor = e.deltaY > 0 ? 1.15 : 1 / 1.15;
      setState((s) => {
        const newUsPerPx = Math.max(10, Math.min(1e9, s.usPerPx * factor));
        // Keep us at cursor fixed across zoom
        const newPan = usAtCursor - (x - 60) * newUsPerPx;
        return { ...s, usPerPx: newUsPerPx, panUs: newPan, followLive: false };
      });
    } else {
      setState((s) => ({ ...s, panUs: s.panUs + e.deltaX * s.usPerPx, followLive: false }));
    }
  };

  function toggleLane(lane: Lane) {
    setState((s) => ({ ...s, lanes: { ...s.lanes, [lane]: !s.lanes[lane] } }));
  }

  function jumpSpan(dir: 1 | -1) {
    const ph = state().playheadUs;
    const ordered = dir === 1
      ? p.spans.filter((s) => Number(s.startTsMicros) > ph).sort((a, b) => Number(a.startTsMicros) - Number(b.startTsMicros))
      : p.spans.filter((s) => Number(s.startTsMicros) < ph).sort((a, b) => Number(b.startTsMicros) - Number(a.startTsMicros));
    const next = ordered[0];
    if (next) {
      setState((s) => ({
        ...s,
        playheadUs: Number(next.startTsMicros),
        selectedEvtId: Number(next.startEvtId),
        followLive: false,
      }));
      p.onSelect?.(Number(next.startEvtId));
    }
  }

  function jumpEvent(dir: 1 | -1) {
    const ph = state().playheadUs;
    const ordered = dir === 1
      ? p.events.filter((e) => Number(e.tsMicros) > ph)
      : p.events.filter((e) => Number(e.tsMicros) < ph).reverse();
    const next = ordered[0];
    if (next) {
      setState((s) => ({
        ...s,
        playheadUs: Number(next.tsMicros),
        selectedEvtId: Number(next.id),
        followLive: false,
      }));
      p.onSelect?.(Number(next.id));
    }
  }

  function fit() {
    const min = Number(p.minTs);
    const max = Number(p.maxTs);
    if (min === 0 || max === 0) return;
    const w = width();
    const total = Math.max(max - min, 1_000_000);
    setState((s) => ({
      ...s,
      panUs: min - Math.round(total * 0.025),
      usPerPx: (total * 1.05) / Math.max(1, w - 60),
      playheadUs: max,
      followLive: true,
    }));
  }

  // M12.S3: replay animation. `playSpeed` is null when stopped, else the
  // playback rate multiplier. We use rAF deltas so playback stays smooth
  // even on background tabs (browsers throttle to 1Hz there; pause then).
  let lastFrameMs = 0;
  let rafId: number | null = null;
  const tickReplay = (now: number) => {
    rafId = null;
    const speed = playSpeed();
    if (speed === null) return;
    const dt = lastFrameMs > 0 ? now - lastFrameMs : 16;
    lastFrameMs = now;
    const dtUs = Math.round(dt * 1000 * speed);
    const max = Number(p.maxTs);
    setState((s) => {
      const next = s.playheadUs + dtUs;
      if (next >= max) {
        // Reached the end — stop and pin to max.
        queueMicrotask(() => setPlaySpeed(null));
        return { ...s, playheadUs: max, followLive: false };
      }
      return { ...s, playheadUs: next, followLive: false };
    });
    // Fire onSelect with the event closest in time so the transcript
    // panel scrolls along during replay.
    const phUs = state().playheadUs;
    let closest = 0;
    let bestDelta = Number.POSITIVE_INFINITY;
    for (const ev of p.events) {
      const t = Number(ev.tsMicros);
      if (t > phUs) break; // events are id-ordered which is also ts-ordered
      const d = phUs - t;
      if (d < bestDelta) {
        bestDelta = d;
        closest = Number(ev.id);
      }
    }
    if (closest !== 0) p.onSelect?.(closest);
    rafId = requestAnimationFrame(tickReplay);
  };

  createEffect(() => {
    const speed = playSpeed();
    if (speed !== null) {
      // Starting / restarting playback. If we're at max already, jump back to min.
      const min = Number(p.minTs);
      const max = Number(p.maxTs);
      if (state().playheadUs >= max && min < max) {
        setState((s) => ({ ...s, playheadUs: min, followLive: false }));
      }
      lastFrameMs = 0;
      if (rafId === null) {
        rafId = requestAnimationFrame(tickReplay);
      }
    } else if (rafId !== null) {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
  });

  onCleanup(() => {
    if (rafId !== null) cancelAnimationFrame(rafId);
  });

  const togglePlay = () => setPlaySpeed((s) => (s === null ? 1 : null));
  const setSpeed = (n: number) => setPlaySpeed(n);

  return (
    <div class="timeline-root" ref={container} style="width: 100%">
      <Show
        when={p.events.length > 0}
        fallback={<p class="dim" style="padding: 0.5rem 0">no events yet</p>}
      >
        <canvas
          ref={canvas}
          onPointerDown={onPointerDownStart}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onWheel={onWheel}
          style="display: block; width: 100%; touch-action: none; user-select: none"
        />
        <div class="timeline-replay">
          <button
            type="button"
            class="btn ghost"
            onClick={togglePlay}
            title="Toggle replay (animates the playhead from start to end)"
          >
            {playSpeed() !== null ? '⏸ pause' : '▶ replay'}
          </button>
          <Show when={playSpeed() !== null}>
            <span class="dim" style="font-size: 11px">speed</span>
            <For each={[1, 2, 4, 8]}>
              {(s) => (
                <button
                  type="button"
                  class={`btn ghost ${playSpeed() === s ? 'active' : ''}`}
                  style="padding: 0.15rem 0.5rem"
                  onClick={() => setSpeed(s)}
                >
                  {s}×
                </button>
              )}
            </For>
          </Show>
        </div>
        <div class="timeline-help dim" style="font-size: 11px; padding: 0.3rem 0">
          drag playhead · scroll pan · ctrl+scroll zoom · click span ·
          [ ] prev/next span · j k prev/next event · 1-4 lanes · f fit · l live · esc clear
        </div>
      </Show>
    </div>
  );
};

export default Timeline;
