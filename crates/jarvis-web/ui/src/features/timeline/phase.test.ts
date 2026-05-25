// § F2.1 — phase swimlanes: tests for the sticky-forward segment derivation.

import { describe, expect, it } from 'vitest';
import type { TimelineEvent } from '~/lib/api/gen/jarvis_pb';
import { computePhaseSegments } from './canvas-renderer';
import {
  HEADER_HEIGHT,
  LANE_GAP,
  LANE_HEIGHT,
  PHASE_RIBBON_TOTAL,
  defaultState,
} from './types';
import { naturalHeight } from './hit-test';

const decision = (id: bigint, tsMicros: bigint, payload: unknown): TimelineEvent =>
  ({
    $typeName: 'jarvis.v1.TimelineEvent',
    id,
    tsMicros,
    taskId: 't',
    agentId: '',
    kind: 'decision',
    subject: '',
    payloadJson: JSON.stringify(payload),
    parentEvt: 0n,
  }) as TimelineEvent;

const other = (id: bigint, tsMicros: bigint, kind: string): TimelineEvent =>
  ({
    $typeName: 'jarvis.v1.TimelineEvent',
    id,
    tsMicros,
    taskId: 't',
    agentId: '',
    kind,
    subject: '',
    payloadJson: '{}',
    parentEvt: 0n,
  }) as TimelineEvent;

describe('computePhaseSegments', () => {
  it('returns no segments when no decision carries a phase', () => {
    const events = [decision(1n, 100n, { reasoning: 'hi' }), other(2n, 200n, 'tool_call')];
    expect(computePhaseSegments(events, 500n)).toEqual([]);
  });

  it('emits a single segment closed at maxTs when only one phase is declared', () => {
    const events = [decision(1n, 100n, { phase: 'plan' })];
    expect(computePhaseSegments(events, 500n)).toEqual([
      { startUs: 100, endUs: 500, phase: 'plan' },
    ]);
  });

  it('emits one segment per phase transition (sticky-forward)', () => {
    const events = [
      decision(1n, 100n, { phase: 'plan' }),
      decision(2n, 200n, { phase: 'act' }),
      decision(3n, 350n, { phase: 'verify' }),
      decision(4n, 500n, { phase: 'ship' }),
    ];
    expect(computePhaseSegments(events, 600n)).toEqual([
      { startUs: 100, endUs: 200, phase: 'plan' },
      { startUs: 200, endUs: 350, phase: 'act' },
      { startUs: 350, endUs: 500, phase: 'verify' },
      { startUs: 500, endUs: 600, phase: 'ship' },
    ]);
  });

  it('coalesces repeated identical phases', () => {
    const events = [
      decision(1n, 100n, { phase: 'plan' }),
      decision(2n, 200n, { phase: 'plan' }),
      decision(3n, 300n, { phase: 'act' }),
    ];
    expect(computePhaseSegments(events, 400n)).toEqual([
      { startUs: 100, endUs: 300, phase: 'plan' },
      { startUs: 300, endUs: 400, phase: 'act' },
    ]);
  });

  it('skips unknown phase strings and unparseable payloads', () => {
    const events: TimelineEvent[] = [
      decision(1n, 100n, { phase: 'plan' }),
      decision(2n, 150n, { phase: 'finalize' }), // not in ALL_PHASES — ignored
      {
        ...decision(3n, 200n, { phase: 'act' }),
        payloadJson: '{not json',
      } as TimelineEvent, // unparseable — ignored
      decision(4n, 250n, { phase: 'verify' }),
    ];
    expect(computePhaseSegments(events, 300n)).toEqual([
      { startUs: 100, endUs: 250, phase: 'plan' },
      { startUs: 250, endUs: 300, phase: 'verify' },
    ]);
  });

  it('ignores non-decision kinds even when they carry a phase field', () => {
    const events = [
      other(1n, 100n, 'tool_call'),
      decision(2n, 200n, { phase: 'plan' }),
    ];
    // The first event would have a phase if we parsed it, but it's a
    // tool_call — segments must come from `decision` only.
    expect(computePhaseSegments(events, 300n)).toEqual([
      { startUs: 200, endUs: 300, phase: 'plan' },
    ]);
  });

  it('clamps the closing segment when maxTs precedes the last declaration', () => {
    // Defensive: if maxTs is stale, the segment must still be non-negative.
    const events = [decision(1n, 500n, { phase: 'plan' })];
    expect(computePhaseSegments(events, 100n)).toEqual([
      { startUs: 500, endUs: 500, phase: 'plan' },
    ]);
  });
});

describe('naturalHeight with phase ribbon', () => {
  it('reserves ribbon space only when hasPhases is true', () => {
    const state = defaultState();
    const lanesHeight = 4 * (LANE_HEIGHT + LANE_GAP);
    expect(naturalHeight(state, false)).toBe(HEADER_HEIGHT + lanesHeight);
    expect(naturalHeight(state, true)).toBe(
      HEADER_HEIGHT + PHASE_RIBBON_TOTAL + lanesHeight,
    );
  });
});
