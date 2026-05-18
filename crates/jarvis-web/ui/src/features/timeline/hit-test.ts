// Hit testing: pointer (cssX, cssY) → event id (or 0 if none).
//
// We index events / spans by ts_micros (already sorted by the server) so we
// can binary-search a time range, then linear-scan within.

import type { TimelineEvent, TimelineSpan } from '~/lib/api/gen/jarvis_pb';
import {
  ALL_LANES,
  HEADER_HEIGHT,
  LANE_GAP,
  LANE_HEIGHT,
  POINT_RADIUS,
  SPAN_HEIGHT,
  type Lane,
  type TimelineState,
} from './types';
import { pointEventLane, usToPx, visibleLanes } from './canvas-renderer';

export function hitTest(
  cssX: number,
  cssY: number,
  events: TimelineEvent[],
  spans: TimelineSpan[],
  state: TimelineState,
): number {
  // Which lane was clicked?
  const lanes = visibleLanes(state);
  const lane = laneAt(cssY, lanes);
  if (!lane) return 0;

  // Check spans on this lane first (they cover wider areas).
  for (const s of spans) {
    if (s.lane !== lane) continue;
    const x0 = usToPx(Number(s.startTsMicros), state);
    const endTs = Number(s.endTsMicros) || (state.panUs + Number.MAX_SAFE_INTEGER);
    const x1 = usToPx(endTs, state);
    const laneCenter = lanes.find((l) => l.lane === lane)!.y + LANE_HEIGHT / 2;
    if (cssX >= x0 && cssX <= x1 && Math.abs(cssY - laneCenter) <= SPAN_HEIGHT / 2 + 2) {
      return Number(s.startEvtId);
    }
  }

  // Then check point events on this lane.
  for (const e of events) {
    if (pointEventLane(e.kind) !== lane) continue;
    const x = usToPx(Number(e.tsMicros), state);
    const laneCenter = lanes.find((l) => l.lane === lane)!.y + LANE_HEIGHT / 2;
    const dx = cssX - x;
    const dy = cssY - laneCenter;
    if (dx * dx + dy * dy <= (POINT_RADIUS + 2) * (POINT_RADIUS + 2)) {
      return Number(e.id);
    }
  }
  return 0;
}

function laneAt(cssY: number, lanes: ReturnType<typeof visibleLanes>): Lane | null {
  if (cssY < HEADER_HEIGHT) return null;
  for (const { lane, y } of lanes) {
    if (cssY >= y && cssY < y + LANE_HEIGHT) return lane;
  }
  return null;
}

/**
 * Distance from the playhead handle (top triangle, ~10px wide centered on
 * the playhead x). Returns true when the pointer is close enough to grab.
 */
export function isOnPlayhead(cssX: number, cssY: number, state: TimelineState): boolean {
  if (cssY > HEADER_HEIGHT + 8) return false;
  const px = usToPx(state.playheadUs, state);
  return Math.abs(cssX - px) <= 6;
}

/** Compute the canvas's natural total height for N visible lanes. */
export function naturalHeight(state: TimelineState): number {
  const n = ALL_LANES.filter((l) => state.lanes[l]).length;
  return HEADER_HEIGHT + n * (LANE_HEIGHT + LANE_GAP);
}
