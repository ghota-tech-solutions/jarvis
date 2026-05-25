// F1.2 — pure-function tests for the DAG layout.
//
// We don't go through the proto `create()` builder — every required field
// is set explicitly so a future schema bump is visible here.

import { describe, expect, it } from 'vitest';
import type { FleetEdge, FleetNode } from '~/lib/api/gen/jarvis_pb';
import { layoutFleet, defaultLayoutOptions } from './layout';

const node = (taskId: string, createdMicros: bigint): FleetNode =>
  ({
    $typeName: 'jarvis.v1.FleetNode',
    taskId,
    shortId: taskId.slice(0, 8),
    status: 'running',
    goal: '',
    workdir: '',
    sandbox: '',
    tokensIn: 0n,
    tokensOut: 0n,
    estimatedCostUsd: 0,
    createdAtMicros: createdMicros,
    updatedAtMicros: createdMicros,
    needsAttention: false,
  }) as FleetNode;

const edge = (parent: string, child: string): FleetEdge =>
  ({
    $typeName: 'jarvis.v1.FleetEdge',
    parentTaskId: parent,
    childTaskId: child,
  }) as FleetEdge;

describe('layoutFleet', () => {
  it('returns an empty layout for an empty graph', () => {
    const out = layoutFleet([], []);
    expect(out).toEqual({ nodes: [], edges: [], width: 0, height: 0 });
  });

  it('places roots at level 0 and children at level 1, sorted by created_at within a level', () => {
    // Two roots (A is older than B), one child of A.
    const nodes: FleetNode[] = [
      node('B', 200n),
      node('A', 100n),
      node('A1', 300n),
    ];
    const edges: FleetEdge[] = [edge('A', 'A1')];

    const out = layoutFleet(nodes, edges, defaultLayoutOptions);

    const byId = new Map(out.nodes.map((p) => [p.node.taskId, p]));
    const a = byId.get('A')!;
    const b = byId.get('B')!;
    const a1 = byId.get('A1')!;

    // Same level → same x.
    expect(a.x).toBe(b.x);
    // Child at level 1 → x is one column over.
    expect(a1.x).toBeGreaterThan(a.x);
    // A is older than B → A is on top.
    expect(a.y).toBeLessThan(b.y);
    // One edge in, one edge out (the only one we passed).
    expect(out.edges).toHaveLength(1);
    expect(out.edges[0].path.startsWith('M')).toBe(true);
  });

  it('treats orphan nodes (no edge to a root) as level-0 nodes', () => {
    // C has an edge from a phantom parent that doesn't exist in `nodes`.
    // The layout should still place C at level 0 (it has no inbound
    // edge from a known node).
    const nodes: FleetNode[] = [node('C', 100n)];
    const edges: FleetEdge[] = [edge('GHOST', 'C')];

    const out = layoutFleet(nodes, edges, defaultLayoutOptions);

    // C has a parentOf entry pointing at GHOST, so it's NOT a root in the
    // BFS sense — but the orphan-recovery pass at the bottom of layoutFleet
    // still places it at level 0.
    expect(out.nodes).toHaveLength(1);
    expect(out.nodes[0].node.taskId).toBe('C');
    expect(out.nodes[0].x).toBe(defaultLayoutOptions.pad);
    // The unresolved edge is dropped (no positioned-edge for it).
    expect(out.edges).toHaveLength(0);
  });
});
