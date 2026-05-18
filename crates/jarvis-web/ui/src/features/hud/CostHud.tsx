// Cost + sandbox HUD — fixed at the bottom of the viewport.
//
// Aggregates over the live FleetUpdate stream so the totals reflect ALL
// tasks (running + completed) at once. The sandbox badge surfaces the
// most "dangerous" sandbox among currently running tasks:
//   green  → only read_only / network=none
//   yellow → workspace_write or egress_only
//   red    → danger_full_access OR net=full

import { Show, createMemo, type Component } from 'solid-js';
import { useFleetStream } from '~/lib/api/streams';
import { statusQuery } from '~/lib/api/queries';
import { createQuery } from '@tanstack/solid-query';

type SandboxRisk = 'green' | 'yellow' | 'red' | 'idle';

const sandboxRiskColor = (r: SandboxRisk): string => {
  switch (r) {
    case 'green': return 'var(--good)';
    case 'yellow': return 'var(--warn)';
    case 'red': return 'var(--error)';
    default: return 'var(--fade)';
  }
};

const sandboxRiskLabel = (r: SandboxRisk): string => {
  switch (r) {
    case 'green': return 'isolated';
    case 'yellow': return 'workspace';
    case 'red': return 'unsandboxed';
    default: return 'idle';
  }
};

const CostHud: Component = () => {
  const fleet = useFleetStream();
  const statusQ = createQuery(statusQuery);

  const stats = createMemo(() => {
    const snap = fleet.value();
    if (!snap) return { tasksRunning: 0, tasksTotal: 0, costUsd: 0, tokensIn: 0n, tokensOut: 0n };
    let tasksRunning = 0;
    let costUsd = 0;
    let tokensIn = 0n;
    let tokensOut = 0n;
    for (const n of snap.nodes) {
      if (n.status === 'running' || n.status === 'pending') tasksRunning++;
      costUsd += n.estimatedCostUsd;
      tokensIn += n.tokensIn;
      tokensOut += n.tokensOut;
    }
    return {
      tasksRunning,
      tasksTotal: snap.nodes.length,
      costUsd,
      tokensIn,
      tokensOut,
    };
  });

  const activeModel = createMemo(() => {
    const models = statusQ.data?.models ?? [];
    return models.find((m) => m.online && !m.quarantined) ?? null;
  });

  const sandboxRisk = createMemo<SandboxRisk>(() => {
    const snap = fleet.value();
    if (!snap) return 'idle';
    let worst: SandboxRisk = 'idle';
    for (const n of snap.nodes) {
      if (n.status !== 'running' && n.status !== 'pending') continue;
      // Without a per-task net_policy/sandbox_mode field on FleetNode we
      // infer risk from the sandbox kind alone. M7.1 will expose the
      // full picture (then this branch can promote to 'red' for
      // net=full or danger_full_access).
      const r: SandboxRisk = n.sandbox === 'docker' ? 'green' : 'yellow';
      if (r === 'yellow') worst = 'yellow';
      else if (worst === 'idle') worst = r;
    }
    return worst;
  });

  return (
    <div class="hud">
      <div class="hud-slot" title="Active model (highest priority, not quarantined)">
        <span class="hud-key">model</span>
        <Show when={activeModel()} fallback={<span class="dim">none</span>}>
          {(m) => (
            <>
              <span class={`dot ${m().online ? 'good' : 'offline'}`} />
              <span>{m().name}</span>
              <span class="fade" style="margin-left: 0.3rem">{m().kind}</span>
            </>
          )}
        </Show>
      </div>

      <div class="hud-slot" title="Tasks currently running or pending">
        <span class="hud-key">running</span>
        <span class={stats().tasksRunning > 0 ? 'warn' : 'dim'}>
          {stats().tasksRunning} / {stats().tasksTotal}
        </span>
      </div>

      <div class="hud-slot" title="Aggregate token usage across the fleet">
        <span class="hud-key">tokens</span>
        <span class="dim">
          {formatNumber(Number(stats().tokensIn))} in
        </span>
        <span class="fade">·</span>
        <span class="dim">
          {formatNumber(Number(stats().tokensOut))} out
        </span>
      </div>

      <div class="hud-slot" title="Aggregate estimated cost (remote models only)">
        <span class="hud-key">cost</span>
        <span class={stats().costUsd > 0.01 ? 'warn' : 'dim'}>
          ${stats().costUsd.toFixed(4)}
        </span>
      </div>

      <div class="hud-slot hud-sandbox" title="Worst-case sandbox among running tasks">
        <span class="hud-key">sandbox</span>
        <span class="dot" style={`background: ${sandboxRiskColor(sandboxRisk())}`} />
        <span style={`color: ${sandboxRiskColor(sandboxRisk())}`}>
          {sandboxRiskLabel(sandboxRisk())}
        </span>
      </div>

      <div class="hud-slot" style="margin-left: auto" title="Fleet stream status">
        <span class={`dot ${fleet.connected() ? 'good' : 'offline'}`} />
        <span class="dim">{fleet.connected() ? 'live' : 'reconnecting'}</span>
      </div>
    </div>
  );
};

function formatNumber(n: number): string {
  if (n < 1_000) return n.toString();
  if (n < 1_000_000) return (n / 1_000).toFixed(1) + 'k';
  return (n / 1_000_000).toFixed(2) + 'M';
}

export default CostHud;
