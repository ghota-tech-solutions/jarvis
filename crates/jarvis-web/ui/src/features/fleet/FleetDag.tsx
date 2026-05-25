import { For, Show, createMemo, type Component } from 'solid-js';
import { A } from '@solidjs/router';
import { useFleetStream } from '~/lib/api/streams';
import { defaultLayoutOptions, layoutFleet, type PositionedNode } from './layout';
import { EmptyState } from '~/components/EmptyState';
import { useT } from '~/lib/i18n';

const statusColor = (s: string): string => {
  switch (s) {
    case 'completed': return 'var(--good)';
    case 'running':
    case 'pending':
      return 'var(--warn)';
    case 'failed': return 'var(--error)';
    case 'cancelled': return 'var(--fade)';
    default: return 'var(--dim)';
  }
};

const Node: Component<{ pn: PositionedNode }> = (p) => {
  const opts = defaultLayoutOptions;
  return (
    <g>
      <A href={`/task/${p.pn.node.taskId}`}>
        <rect
          x={p.pn.x}
          y={p.pn.y}
          width={opts.nodeWidth}
          height={opts.nodeHeight}
          rx={4}
          fill="var(--bg-elev)"
          stroke={p.pn.node.needsAttention ? 'var(--error)' : 'var(--border)'}
          stroke-width={p.pn.node.needsAttention ? 2 : 1}
        />
        <text
          x={p.pn.x + 10}
          y={p.pn.y + 18}
          fill={statusColor(p.pn.node.status)}
          font-size="11"
          font-family="ui-monospace, monospace"
        >
          {p.pn.node.status}
        </text>
        <text
          x={p.pn.x + opts.nodeWidth - 10}
          y={p.pn.y + 18}
          fill="var(--dim)"
          font-size="11"
          font-family="ui-monospace, monospace"
          text-anchor="end"
        >
          {p.pn.node.shortId}
        </text>
        <text
          x={p.pn.x + 10}
          y={p.pn.y + 38}
          fill="var(--body)"
          font-size="12"
          font-family="system-ui, sans-serif"
        >
          {p.pn.node.goal.length > 32 ? p.pn.node.goal.slice(0, 31) + '…' : p.pn.node.goal}
        </text>
        <text
          x={p.pn.x + 10}
          y={p.pn.y + 56}
          fill="var(--dim)"
          font-size="10"
          font-family="ui-monospace, monospace"
        >
          {p.pn.node.sandbox || 'native'}
          {p.pn.node.estimatedCostUsd > 0 && (
            <>
              {' · $'}
              {p.pn.node.estimatedCostUsd.toFixed(3)}
            </>
          )}
        </text>
      </A>
    </g>
  );
};

const FleetDag: Component = () => {
  const t = useT();
  const stream = useFleetStream();

  const layout = createMemo(() => {
    const snap = stream.value();
    if (!snap) return null;
    return layoutFleet(snap.nodes, snap.edges);
  });

  return (
    <section>
      <header style="display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.7rem">
        <h2 class="heading" style="margin: 0">Fleet</h2>
        <span class={`dot ${stream.connected() ? 'good' : 'offline'}`} />
        <span class="dim" style="font-size: 12px">
          {stream.connected() ? 'live' : 'reconnecting'}
        </span>
        <Show when={stream.error()}>
          <span class="error" style="font-size: 12px">
            {String(stream.error())}
          </span>
        </Show>
        <span style="margin-left: auto" class="dim">
          {layout() ? `${layout()!.nodes.length} nodes · ${layout()!.edges.length} edges` : '…'}
        </span>
      </header>
      <Show when={layout()} fallback={<p class="dim">waiting for fleet snapshot…</p>}>
        {(l) => (
          <Show
            when={l().nodes.length > 0}
            fallback={
              <EmptyState
                title={t().empty.no_fleet_title}
                hint={t().empty.no_fleet_hint}
                actionHref="/"
                actionLabel={t().empty.no_fleet_cta}
              />
            }
          >
            <div style="overflow: auto; border: 1px solid var(--border); border-radius: 4px; background: var(--bg)">
              <svg
                width={l().width}
                height={Math.max(l().height, 200)}
                style="display: block"
              >
                <defs>
                  <marker
                    id="arrow"
                    viewBox="0 0 10 10"
                    refX="8"
                    refY="5"
                    markerWidth="5"
                    markerHeight="5"
                    orient="auto"
                  >
                    <path d="M0,0 L10,5 L0,10 z" fill="var(--fade)" />
                  </marker>
                </defs>
                <For each={l().edges}>
                  {(pe) => (
                    <path
                      d={pe.path}
                      fill="none"
                      stroke="var(--fade)"
                      stroke-width="1.5"
                      marker-end="url(#arrow)"
                    />
                  )}
                </For>
                <For each={l().nodes}>{(pn) => <Node pn={pn} />}</For>
              </svg>
            </div>
          </Show>
        )}
      </Show>
    </section>
  );
};

export default FleetDag;
