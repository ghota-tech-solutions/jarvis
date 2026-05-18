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
});

// Visual constants
export const LANE_HEIGHT = 28;
export const LANE_GAP = 6;
export const HEADER_HEIGHT = 22;
export const SPAN_HEIGHT = 14;
export const POINT_RADIUS = 4;
