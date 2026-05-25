// Types shared between the canvas renderer, hit-test, and the Solid view.

export type Lane = 'tools' | 'llm' | 'plan' | 'verdict';

export const ALL_LANES: Lane[] = ['tools', 'llm', 'plan', 'verdict'];

export interface TimelineState {
  // Microsecond timestamp at the left edge of the canvas viewport.
  panUs: number;
  // Microseconds-per-pixel. Smaller = more zoomed in.
  usPerPx: number;
  // Playhead in microsecond timestamp.
  playheadUs: number;
  // Whether the playhead is pinned to "now" (right edge); scrubbing back disables.
  followLive: boolean;
  // Lane visibility — toggle via 1..4.
  lanes: Record<Lane, boolean>;
  // Currently selected event id (0 = none).
  selectedEvtId: number;
}

export const defaultState = (): TimelineState => ({
  panUs: 0,
  usPerPx: 1000, // 1 ms / px → ~1s per 1000px viewport
  playheadUs: 0,
  followLive: true,
  lanes: { tools: true, llm: true, plan: true, verdict: true },
  selectedEvtId: 0,
});

export interface ThemeColors {
  bg: string;
  panel: string;
  border: string;
  body: string;
  dim: string;
  fade: string;
  accent: string;
  good: string;
  warn: string;
  error: string;
  llm: string;
  // § F2.1 — phase swimlane palette. One color per Plan/Act/Verify/Ship,
  // chosen to read at a glance even when the ribbon is only 12 px tall.
  phasePlan: string;
  phaseAct: string;
  phaseVerify: string;
  phaseShip: string;
}

export const defaultTheme = (): ThemeColors => ({
  bg: '#0a0a0a',
  panel: '#121212',
  border: '#282828',
  body: '#c4c4c4',
  dim: '#808080',
  fade: '#505050',
  accent: '#d4b478',
  good: '#8cb46e',
  warn: '#dcaf5a',
  error: '#dc6e5a',
  llm: '#b4c8dc',
  // Mirrors the SPA's task-header phase pill palette so the timeline
  // ribbon and the header chip read as the same signal.
  phasePlan: '#7aa2c8',
  phaseAct: '#dcaf5a',
  phaseVerify: '#8cb46e',
  phaseShip: '#d4b478',
});

// Visual constants
export const LANE_HEIGHT = 28;
export const LANE_GAP = 6;
export const HEADER_HEIGHT = 22;
export const SPAN_HEIGHT = 14;
export const POINT_RADIUS = 4;
// § F2.1 — phase ribbon dimensions. Inserted between the header tick
// strip and the first lane only when at least one phase has been
// declared on the task; otherwise lanes sit flush against the header.
export const PHASE_RIBBON_HEIGHT = 12;
export const PHASE_RIBBON_GAP = 4;
export const PHASE_RIBBON_TOTAL = PHASE_RIBBON_HEIGHT + PHASE_RIBBON_GAP;

export type Phase = 'plan' | 'act' | 'verify' | 'ship';
export const ALL_PHASES: Phase[] = ['plan', 'act', 'verify', 'ship'];

export interface PhaseSegment {
  startUs: number;
  endUs: number;
  phase: Phase;
}

export function phaseColor(phase: Phase, theme: ThemeColors): string {
  switch (phase) {
    case 'plan':
      return theme.phasePlan;
    case 'act':
      return theme.phaseAct;
    case 'verify':
      return theme.phaseVerify;
    case 'ship':
      return theme.phaseShip;
  }
}
