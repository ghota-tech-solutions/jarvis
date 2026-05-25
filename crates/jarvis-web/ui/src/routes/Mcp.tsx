// § F2.7 — MCP Tool Explorer.
//
// Renders the connected MCP servers and their published tool catalogs
// (read from the existing `GetStatus.mcp_servers` field — no new RPC).
// Each tool is listed with its auto-qualified name (`<server>.<tool>`)
// and grouped under its server card so the user gets a quick map of
// what's available to the agent.
//
// "Try this tool" is intentionally out of scope for v1 — invoking a
// tool standalone requires a new `InvokeTool` RPC + careful sandbox
// gating. Tracked as a follow-up.

import { For, Show, type Component } from 'solid-js';
import { createQuery } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';
import AppErrorBoundary from '~/components/ErrorBoundary';
import { EmptyState } from '~/components/EmptyState';

const Mcp: Component = () => {
  const statusQ = createQuery(() => ({
    queryKey: ['daemon-status'],
    queryFn: async () => await jarvis.getStatus({}),
    refetchInterval: 10_000,
  }));

  const servers = () => statusQ.data?.mcpServers ?? [];

  return (
    <AppErrorBoundary name="Mcp">
      <section class="mcp-page">
        <header class="mcp-header">
          <h2 class="heading" style="margin: 0">
            MCP servers
          </h2>
          <p class="dim" style="margin: 0.2rem 0 0 0; font-size: 12px">
            External tool servers connected to the daemon over stdio JSON-RPC.
            Tools are auto-qualified as <code>&lt;server&gt;.&lt;tool&gt;</code>{' '}
            so the agent can call them like any builtin.
          </p>
        </header>

        <Show
          when={servers().length > 0}
          fallback={
            <EmptyState
              title="No MCP servers configured"
              hint="Add a [mcp.servers.<name>] block to your jarvis.toml to plug an external MCP server (filesystem, github, postgres, …). The daemon discovers tools automatically on startup."
            />
          }
        >
          <div class="mcp-grid">
            <For each={servers()}>
              {(s) => (
                <div class={`mcp-card ${s.connected ? 'is-connected' : 'is-down'}`}>
                  <header class="mcp-card-header">
                    <span class="mcp-card-name">{s.name}</span>
                    <span
                      class={`pill ${s.connected ? 'good' : 'error'}`}
                      style="font-size: 10px"
                    >
                      {s.connected ? 'connected' : 'down'}
                    </span>
                  </header>
                  <Show when={s.error}>
                    <p class="error" style="margin: 0.3rem 0; font-size: 12px">
                      {s.error}
                    </p>
                  </Show>
                  <p class="dim" style="margin: 0.3rem 0; font-size: 11px">
                    {s.tools.length} tool{s.tools.length === 1 ? '' : 's'} exposed
                  </p>
                  <Show when={s.tools.length > 0}>
                    <ul class="mcp-tools">
                      <For each={s.tools}>
                        {(name) => (
                          <li class="mcp-tool">
                            <code>{name}</code>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                </div>
              )}
            </For>
          </div>
        </Show>
      </section>
    </AppErrorBoundary>
  );
};

export default Mcp;
