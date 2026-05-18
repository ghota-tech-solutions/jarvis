import { Show, type Component } from 'solid-js';
import { createQuery } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';

const Dashboard: Component = () => {
  const ping = createQuery(() => ({
    queryKey: ['ping'],
    queryFn: async () => {
      const res = await jarvis.ping({});
      return res;
    },
    refetchInterval: 10_000,
  }));

  return (
    <section>
      <h1 style="color: var(--heading)">Dashboard</h1>
      <p class="dim">Tasks list, projects sidebar, models, MCP — wired in M6.S9.</p>
      <div style="margin-top: 1.5rem; padding: 0.75rem 1rem; border: 1px solid var(--fade); border-radius: 4px; background: var(--bg-elev)">
        <strong style="color: var(--accent)">Daemon</strong>
        <Show
          when={!ping.isPending}
          fallback={<span class="dim"> · pinging…</span>}
        >
          <Show
            when={!ping.error}
            fallback={
              <span style="color: var(--error)"> · {String(ping.error)}</span>
            }
          >
            <span class="dim"> · </span>
            <code>v{ping.data?.version}</code>
            <span class="dim"> · up </span>
            <code>{Number(ping.data?.uptimeSeconds ?? 0n)}s</code>
          </Show>
        </Show>
      </div>
    </section>
  );
};

export default Dashboard;
