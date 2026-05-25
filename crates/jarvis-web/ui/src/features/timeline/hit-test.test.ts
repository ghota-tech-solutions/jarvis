// F1.2 — hit-test + geometry helpers.
//
// Note: the F1.2 spec asks for a "binary search returns correct event"
// test, but the current `hitTest` impl is a linear scan (see hit-test.ts).
// We test what's actually implemented and flag the divergence in the
// scaffold report. If/when hit-test is rewritten to binary-search by
// ts_micros, port these tests to assert O(log n) correctness too.

import { describe, expect, it } from 'vitest';
import type { TimelineEvent } from '~/lib/api/gen/jarvis_pb';
import { hitTest, isOnPlayhead, naturalHeight } from './hit-test';
import {
  HEADER_HEIGHT,
  LANE_GAP,
  LANE_HEIGHT,
  defaultState,
  type TimelineState,
} from './types';

const ev = (id: bigint, tsMicros: bigint, kind: string): TimelineEvent =>
  ({
    $typeName: 'jarvis.v1.TimelineEvent',
    id,
    tsMicros,
    taskId: 't',
    agentId: '',
    kind,
    subject: '',
    payloadJson: '',
    parentEvt: 0n,
  }) as TimelineEvent;

const baseState = (): TimelineState => ({
  ...defaultState(),
  panUs: 0,
  usPerPx: 1, // 1 µs per pixel → easy arithmetic
});

describe('hitTest', () => {
  it('returns 0 when there are no events and no spans', () => {
    const state = baseState();
    // Click in the middle of the body area (well inside a lane).
    const yInsideLane = HEADER_HEIGHT + LANE_HEIGHT / 2;
    expect(hitTest(100, yInsideLane, [], [], state)).toBe(0);
  });

  it('returns 0 for a click in the header (above the lanes)', () => {
    const state = baseState();
    const events = [ev(42n, 500n, 'decision')];
    expect(hitTest(500 + 60 /* LANE_LABEL_WIDTH */, HEADER_HEIGHT / 2, events, [], state)).toBe(0);
  });

  it('returns the matching point event id when the click hits it', () => {
    const state = baseState();
    // 'decision' lives on the 'llm' lane, which is the 2nd visible lane
    // (order: tools, llm, plan, verdict). Centre y = HEADER + LANE+GAP +
    // LANE/2.
    const decision = ev(99n, 500n, 'decision');
    // x = LANE_LABEL_WIDTH (60) + (ts - panUs) / usPerPx = 60 + 500 = 560
    const cx = 60 + 500;
    const cy = HEADER_HEIGHT + (LANE_HEIGHT + LANE_GAP) + LANE_HEIGHT / 2;
    expect(hitTest(cx, cy, [decision], [], state)).toBe(99);
  });
});

describe('isOnPlayhead', () => {
  it('detects grabs near the playhead handle in the header band', () => {
    const state = baseState();
    state.playheadUs = 200;
    // x = 60 + 200 = 260 — pointer 1px off is still within the ±6 px grab.
    expect(isOnPlayhead(261, 4, state)).toBe(true);
  });

  it('rejects clicks below the header band', () => {
    const state = baseState();
    state.playheadUs = 200;
    expect(isOnPlayhead(260, HEADER_HEIGHT + 20, state)).toBe(false);
  });
});

describe('naturalHeight', () => {
  it('scales with the number of visible lanes', () => {
    const allOn = defaultState();
    const oneOn: TimelineState = {
      ...defaultState(),
      lanes: { tools: true, llm: false, plan: false, verdict: false },
    };
    expect(naturalHeight(allOn)).toBeGreaterThan(naturalHeight(oneOn));
    // With one lane: HEADER + 1*(LANE+GAP).
    expect(naturalHeight(oneOn)).toBe(HEADER_HEIGHT + (LANE_HEIGHT + LANE_GAP));
  });
});
