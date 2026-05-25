// Pure canvas drawing for the scrubbable reasoning timeline.
//
// Draw order:
//   1. Background + lane separators
//   2. Lane labels (left margin)
//   3. Spans (rectangles) per visible lane
//   4. Point events (small dots) for decisions / errors / heartbeats
//   5. Playhead (vertical line)
//
// The renderer is a pure function — all state comes from arguments.
// Solid's effect re-invokes it whenever state changes.

import type { TimelineEvent, TimelineSpan } from '~/lib/api/gen/jarvis_pb';
import {
  ALL_LANES,
  ALL_PHASES,
  HEADER_HEIGHT,
  LANE_GAP,
  LANE_HEIGHT,
  PHASE_RIBBON_GAP,
  PHASE_RIBBON_HEIGHT,
  PHASE_RIBBON_TOTAL,
  POINT_RADIUS,
  SPAN_HEIGHT,
  phaseColor,
  type Lane,
  type Phase,
  type PhaseSegment,
  type ThemeColors,
  type TimelineState,
} from './types';

const LANE_LABEL_WIDTH = 60;

const isPhase = (s: string): s is Phase =>
  (ALL_PHASES as readonly string[]).includes(s);

/**
 * Walk events in chronological order, picking up `phase` from each
 * decision event's payload and emitting one segment per phase change.
 * The convention is sticky-forward: a phase set at ts T stays active
 * until the next decision overrides it (or until maxTs). Events with
 * unparseable JSON or no `phase` field are skipped silently.
 */
export function computePhaseSegments(
  events: readonly TimelineEvent[],
  maxTs: bigint,
): PhaseSegment[] {
  const segs: PhaseSegment[] = [];
  let active: { phase: Phase; startUs: number } | null = null;
  for (const ev of events) {
    if (ev.kind !== 'decision') continue;
    let next: Phase | null = null;
    try {
      const p = JSON.parse(ev.payloadJson) as { phase?: unknown };
      if (typeof p.phase === 'string' && isPhase(p.phase)) next = p.phase;
    } catch {
      /* unparseable payload — skip */
    }
    if (next === null) continue;
    const t = Number(ev.tsMicros);
    if (active === null) {
      active = { phase: next, startUs: t };
      continue;
    }
    if (active.phase === next) continue;
    segs.push({ startUs: active.startUs, endUs: t, phase: active.phase });
    active = { phase: next, startUs: t };
  }
  if (active !== null) {
    segs.push({
      startUs: active.startUs,
      endUs: Math.max(active.startUs, Number(maxTs)),
      phase: active.phase,
    });
  }
  return segs;
}

/** Vertical offset applied to all lanes when phase segments are present. */
export const phaseOffset = (hasPhases: boolean): number =>
  hasPhases ? PHASE_RIBBON_TOTAL : 0;

export interface RenderInput {
  ctx: CanvasRenderingContext2D;
  widthCss: number;
  heightCss: number;
  dpr: number;
  events: TimelineEvent[];
  spans: TimelineSpan[];
  minTs: bigint;
  maxTs: bigint;
  state: TimelineState;
  theme: ThemeColors;
  hoverEvtId?: number;
  /** Pre-computed phase segments. Empty array hides the ribbon. */
  phaseSegments?: PhaseSegment[];
}

export interface LaneGeom {
  lane: Lane;
  y: number;
}

/**
 * Active visible lanes in render order. The optional `hasPhases` shifts
 * the first lane down by PHASE_RIBBON_TOTAL to leave room for the ribbon.
 */
export function visibleLanes(
  state: TimelineState,
  hasPhases = false,
): LaneGeom[] {
  const out: LaneGeom[] = [];
  let y = HEADER_HEIGHT + phaseOffset(hasPhases);
  for (const lane of ALL_LANES) {
    if (!state.lanes[lane]) continue;
    out.push({ lane, y });
    y += LANE_HEIGHT + LANE_GAP;
  }
  return out;
}

/** Map a microsecond timestamp to a pixel x. */
export const usToPx = (us: number, state: TimelineState): number =>
  LANE_LABEL_WIDTH + (us - state.panUs) / state.usPerPx;

/** Inverse of usToPx — pixel → microsecond timestamp. */
export const pxToUs = (px: number, state: TimelineState): number =>
  state.panUs + (px - LANE_LABEL_WIDTH) * state.usPerPx;

/** Lane assignment for point events (those without a span). */
export function pointEventLane(kind: string): Lane | null {
  switch (kind) {
    case 'decision':
      return 'llm';
    case 'verdict':
    case 'continuation':
    case 'attempt':
      return 'verdict';
    case 'error':
      return 'tools';
    default:
      return null;
  }
}

export function render(input: RenderInput): void {
  const { ctx, widthCss, heightCss, dpr, events, spans, state, theme, hoverEvtId } = input;
  const phaseSegments = input.phaseSegments ?? [];
  const hasPhases = phaseSegments.length > 0;

  // Reset for HiDPI.
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, widthCss, heightCss);

  // Background
  ctx.fillStyle = theme.panel;
  ctx.fillRect(0, 0, widthCss, heightCss);

  drawHeader(ctx, widthCss, theme, state);

  if (hasPhases) {
    drawPhaseRibbon(ctx, phaseSegments, widthCss, state, theme);
  }

  const lanes = visibleLanes(state, hasPhases);
  drawLaneStripes(ctx, lanes, widthCss, theme);
  drawLaneLabels(ctx, lanes, theme);

  // Spans
  for (const span of spans) {
    const startId = Number(span.startEvtId);
    drawSpan(
      ctx, span, lanes, widthCss, state, theme,
      startId === hoverEvtId || startId === state.selectedEvtId,
    );
  }

  // Point events (kinds that don't have spans)
  for (const evt of events) {
    const lane = pointEventLane(evt.kind);
    if (!lane) continue;
    if (!state.lanes[lane]) continue;
    const id = Number(evt.id);
    drawPointEvent(
      ctx, evt, lane, lanes, state, theme,
      id === hoverEvtId || id === state.selectedEvtId,
    );
  }

  drawPlayhead(ctx, heightCss, widthCss, state, theme);
}

function drawHeader(
  ctx: CanvasRenderingContext2D,
  widthCss: number,
  theme: ThemeColors,
  state: TimelineState,
): void {
  ctx.fillStyle = theme.bg;
  ctx.fillRect(0, 0, widthCss, HEADER_HEIGHT);
  ctx.fillStyle = theme.fade;
  ctx.fillRect(0, HEADER_HEIGHT - 1, widthCss, 1);

  // Tick marks every "nice" interval. Pick interval so we get 6–12 ticks.
  const viewportUs = (widthCss - LANE_LABEL_WIDTH) * state.usPerPx;
  const niceIntervals = [
    100, 250, 500, 1000, 2_500, 5_000, 10_000, 25_000, 50_000,
    100_000, 250_000, 500_000, 1_000_000, 5_000_000, 10_000_000,
    30_000_000, 60_000_000, 300_000_000, 600_000_000, 1_800_000_000,
  ];
  const target = viewportUs / 8;
  let interval = niceIntervals[niceIntervals.length - 1];
  for (const n of niceIntervals) {
    if (n >= target) {
      interval = n;
      break;
    }
  }
  const firstTick = Math.ceil(state.panUs / interval) * interval;
  ctx.fillStyle = theme.fade;
  ctx.font = '10px ui-monospace, monospace';
  ctx.textBaseline = 'middle';
  for (let t = firstTick; t < state.panUs + viewportUs + interval; t += interval) {
    const x = usToPx(t, state);
    if (x < LANE_LABEL_WIDTH || x > widthCss) continue;
    ctx.fillRect(x, HEADER_HEIGHT - 6, 1, 5);
    ctx.fillText(formatUs(t - state.panUs), x + 3, HEADER_HEIGHT / 2);
  }
}

function formatUs(us: number): string {
  if (us < 1_000) return `${us}µ`;
  if (us < 1_000_000) return `${(us / 1_000).toFixed(0)}ms`;
  if (us < 60_000_000) return `${(us / 1_000_000).toFixed(1)}s`;
  return `${(us / 60_000_000).toFixed(1)}m`;
}

function drawLaneStripes(
  ctx: CanvasRenderingContext2D,
  lanes: LaneGeom[],
  widthCss: number,
  theme: ThemeColors,
): void {
  for (const { y } of lanes) {
    ctx.fillStyle = theme.border;
    ctx.fillRect(LANE_LABEL_WIDTH, y + LANE_HEIGHT - 1, widthCss - LANE_LABEL_WIDTH, 1);
  }
}

function drawLaneLabels(
  ctx: CanvasRenderingContext2D,
  lanes: LaneGeom[],
  theme: ThemeColors,
): void {
  ctx.fillStyle = theme.dim;
  ctx.font = '10px ui-monospace, monospace';
  ctx.textBaseline = 'middle';
  for (const { lane, y } of lanes) {
    ctx.fillText(lane.toUpperCase(), 4, y + LANE_HEIGHT / 2);
  }
}

function laneY(lane: Lane, lanes: LaneGeom[]): number | null {
  const l = lanes.find((x) => x.lane === lane);
  return l ? l.y : null;
}

function spanColor(outcome: string, theme: ThemeColors): string {
  switch (outcome) {
    case 'ok': return theme.good;
    case 'error': return theme.error;
    case 'running': return theme.warn;
    default: return theme.dim;
  }
}

function drawSpan(
  ctx: CanvasRenderingContext2D,
  span: TimelineSpan,
  lanes: LaneGeom[],
  widthCss: number,
  state: TimelineState,
  theme: ThemeColors,
  highlighted: boolean,
): void {
  const lane = span.lane as Lane;
  const y = laneY(lane, lanes);
  if (y === null) return;
  const start = Number(span.startTsMicros);
  const end = Number(span.endTsMicros) || (state.panUs + (widthCss - LANE_LABEL_WIDTH) * state.usPerPx);
  let x0 = usToPx(start, state);
  let x1 = usToPx(end, state);
  if (x1 < LANE_LABEL_WIDTH || x0 > widthCss) return;
  x0 = Math.max(x0, LANE_LABEL_WIDTH);
  x1 = Math.min(x1, widthCss);
  const w = Math.max(2, x1 - x0);
  const sy = y + (LANE_HEIGHT - SPAN_HEIGHT) / 2;

  ctx.fillStyle = spanColor(span.outcome, theme);
  ctx.globalAlpha = highlighted ? 0.95 : 0.75;
  ctx.fillRect(x0, sy, w, SPAN_HEIGHT);
  ctx.globalAlpha = 1;

  if (highlighted) {
    ctx.strokeStyle = theme.accent;
    ctx.lineWidth = 1;
    ctx.strokeRect(x0 + 0.5, sy + 0.5, w - 1, SPAN_HEIGHT - 1);
  }

  // Label inside the span if it fits (~6 chars per 40px).
  if (w > 60) {
    ctx.fillStyle = theme.bg;
    ctx.font = '11px ui-sans-serif, system-ui, sans-serif';
    ctx.textBaseline = 'middle';
    const maxChars = Math.floor((w - 8) / 6);
    const label = span.label.length > maxChars ? span.label.slice(0, maxChars - 1) + '…' : span.label;
    ctx.fillText(label, x0 + 4, sy + SPAN_HEIGHT / 2);
  }
}

function drawPointEvent(
  ctx: CanvasRenderingContext2D,
  evt: TimelineEvent,
  lane: Lane,
  lanes: LaneGeom[],
  state: TimelineState,
  theme: ThemeColors,
  highlighted: boolean,
): void {
  const y = laneY(lane, lanes);
  if (y === null) return;
  const x = usToPx(Number(evt.tsMicros), state);
  if (x < LANE_LABEL_WIDTH || x > 100_000) return;
  const cy = y + LANE_HEIGHT / 2;
  ctx.beginPath();
  ctx.arc(x, cy, highlighted ? POINT_RADIUS + 1 : POINT_RADIUS, 0, Math.PI * 2);
  ctx.fillStyle = pointEventColor(evt.kind, theme);
  ctx.fill();
  if (highlighted) {
    ctx.strokeStyle = theme.accent;
    ctx.lineWidth = 1.5;
    ctx.stroke();
  }
}

function pointEventColor(kind: string, theme: ThemeColors): string {
  switch (kind) {
    case 'decision': return theme.llm;
    case 'verdict': return theme.good;
    case 'continuation': return theme.warn;
    case 'error': return theme.error;
    case 'attempt': return theme.dim;
    default: return theme.dim;
  }
}

/**
 * Draw the phase ribbon between header and lanes. Each segment is a
 * colored rectangle stretching across its time interval; the phase
 * label is drawn inside when there's room (>32 px). The "PHASE" lane
 * label sits at the left, mirroring how Lane labels are rendered.
 */
function drawPhaseRibbon(
  ctx: CanvasRenderingContext2D,
  segments: readonly PhaseSegment[],
  widthCss: number,
  state: TimelineState,
  theme: ThemeColors,
): void {
  const y = HEADER_HEIGHT + PHASE_RIBBON_GAP / 2;

  // Left margin label.
  ctx.fillStyle = theme.dim;
  ctx.font = '10px ui-monospace, monospace';
  ctx.textBaseline = 'middle';
  ctx.fillText('PHASE', 4, y + PHASE_RIBBON_HEIGHT / 2);

  for (const seg of segments) {
    let x0 = usToPx(seg.startUs, state);
    let x1 = usToPx(seg.endUs, state);
    if (x1 < LANE_LABEL_WIDTH || x0 > widthCss) continue;
    x0 = Math.max(x0, LANE_LABEL_WIDTH);
    x1 = Math.min(x1, widthCss);
    const w = Math.max(2, x1 - x0);
    ctx.fillStyle = phaseColor(seg.phase, theme);
    ctx.globalAlpha = 0.85;
    ctx.fillRect(x0, y, w, PHASE_RIBBON_HEIGHT);
    ctx.globalAlpha = 1;
    if (w > 32) {
      ctx.fillStyle = theme.bg;
      ctx.font = '9px ui-sans-serif, system-ui, sans-serif';
      ctx.textBaseline = 'middle';
      ctx.fillText(seg.phase, x0 + 4, y + PHASE_RIBBON_HEIGHT / 2);
    }
  }
}

function drawPlayhead(
  ctx: CanvasRenderingContext2D,
  heightCss: number,
  widthCss: number,
  state: TimelineState,
  theme: ThemeColors,
): void {
  const x = usToPx(state.playheadUs, state);
  if (x < LANE_LABEL_WIDTH || x > widthCss) return;
  ctx.strokeStyle = theme.accent;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(x + 0.5, HEADER_HEIGHT);
  ctx.lineTo(x + 0.5, heightCss);
  ctx.stroke();
  // Triangle handle at the top
  ctx.fillStyle = theme.accent;
  ctx.beginPath();
  ctx.moveTo(x - 4, HEADER_HEIGHT);
  ctx.lineTo(x + 4, HEADER_HEIGHT);
  ctx.lineTo(x, HEADER_HEIGHT + 5);
  ctx.closePath();
  ctx.fill();
}
