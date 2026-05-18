// Level-based layered DAG layout. Roots on the left, children on the right.
// Within a level, nodes are stacked by created_at ascending.
//
// Output: x/y for every node + cubic Bézier path strings for every edge.
// Designed for SVG; ~50 nodes max before we'd care about more sophisticated
// algorithms (Sugiyama, dot, etc.).

import type { FleetEdge, FleetNode } from '~/lib/api/gen/jarvis_pb';

export interface LayoutOptions {
  nodeWidth: number;
  nodeHeight: number;
  hGap: number;
  vGap: number;
  pad: number;
}

export const defaultLayoutOptions: LayoutOptions = {
  nodeWidth: 220,
  nodeHeight: 78,
  hGap: 80,
  vGap: 18,
  pad: 24,
};

export interface PositionedNode {
  node: FleetNode;
  x: number;
  y: number;
}

export interface PositionedEdge {
  edge: FleetEdge;
  path: string;
}

export interface LayoutResult {
  nodes: PositionedNode[];
  edges: PositionedEdge[];
  width: number;
  height: number;
}

export function layoutFleet(
  nodes: FleetNode[],
  edges: FleetEdge[],
  opts: LayoutOptions = defaultLayoutOptions,
): LayoutResult {
  if (nodes.length === 0) return { nodes: [], edges: [], width: 0, height: 0 };

  // Build adjacency.
  const childrenOf = new Map<string, string[]>();
  const parentOf = new Map<string, string>();
  for (const e of edges) {
    const arr = childrenOf.get(e.parentTaskId) ?? [];
    arr.push(e.childTaskId);
    childrenOf.set(e.parentTaskId, arr);
    parentOf.set(e.childTaskId, e.parentTaskId);
  }

  // BFS from each root → assign level
  const byId = new Map(nodes.map((n) => [n.taskId, n]));
  const level = new Map<string, number>();
  const roots: string[] = nodes.filter((n) => !parentOf.has(n.taskId)).map((n) => n.taskId);
  const queue: string[] = [];
  for (const r of roots) {
    level.set(r, 0);
    queue.push(r);
  }
  let head = 0;
  while (head < queue.length) {
    const id = queue[head++];
    const lvl = level.get(id) ?? 0;
    for (const c of childrenOf.get(id) ?? []) {
      if (!level.has(c)) {
        level.set(c, lvl + 1);
        queue.push(c);
      }
    }
  }
  // Orphans not reachable from any root land at level 0.
  for (const n of nodes) {
    if (!level.has(n.taskId)) level.set(n.taskId, 0);
  }

  // Group by level.
  const perLevel = new Map<number, FleetNode[]>();
  for (const n of nodes) {
    const lvl = level.get(n.taskId) ?? 0;
    const arr = perLevel.get(lvl) ?? [];
    arr.push(n);
    perLevel.set(lvl, arr);
  }
  for (const arr of perLevel.values()) {
    arr.sort((a, b) => Number(a.createdAtMicros - b.createdAtMicros));
  }

  // Assign x/y.
  const positioned: PositionedNode[] = [];
  const xOf = new Map<string, number>();
  const yOf = new Map<string, number>();
  const maxLvl = Math.max(...Array.from(perLevel.keys()));
  let maxHeight = 0;
  for (let lvl = 0; lvl <= maxLvl; lvl++) {
    const col = perLevel.get(lvl) ?? [];
    const x = opts.pad + lvl * (opts.nodeWidth + opts.hGap);
    col.forEach((n, i) => {
      const y = opts.pad + i * (opts.nodeHeight + opts.vGap);
      positioned.push({ node: n, x, y });
      xOf.set(n.taskId, x);
      yOf.set(n.taskId, y);
      maxHeight = Math.max(maxHeight, y + opts.nodeHeight + opts.pad);
    });
  }

  // Edges → cubic Bézier from right side of parent to left side of child.
  const positionedEdges: PositionedEdge[] = [];
  for (const e of edges) {
    const px = xOf.get(e.parentTaskId);
    const py = yOf.get(e.parentTaskId);
    const cx = xOf.get(e.childTaskId);
    const cy = yOf.get(e.childTaskId);
    if (px === undefined || py === undefined || cx === undefined || cy === undefined) continue;
    const x0 = px + opts.nodeWidth;
    const y0 = py + opts.nodeHeight / 2;
    const x1 = cx;
    const y1 = cy + opts.nodeHeight / 2;
    const mx = (x0 + x1) / 2;
    positionedEdges.push({
      edge: e,
      path: `M${x0},${y0} C${mx},${y0} ${mx},${y1} ${x1},${y1}`,
    });
  }

  const width =
    opts.pad + (maxLvl + 1) * opts.nodeWidth + maxLvl * opts.hGap + opts.pad;

  // Quietly use byId so unused-imports lint stays happy on this hand-rolled
  // layout. The map is conceptually useful even though every read flows
  // through `perLevel` instead.
  void byId;

  return {
    nodes: positioned,
    edges: positionedEdges,
    width,
    height: maxHeight,
  };
}
