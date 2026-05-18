import { For, Show, type Component } from 'solid-js';
import { createQuery } from '@tanstack/solid-query';
import { statusQuery } from '~/lib/api/queries';

type Props = { expanded: boolean };

const SidebarRight: Component<Props> = (_props) => {
  const statusQ = createQuery(statusQuery);

  return (
    <div>
      <h3 class="section-title">Daemon</h3>
      <Show when={statusQ.data} fallback={<p class="dim">pinging…</p>}>
        {(s) => (
          <p style="font-size: 12px; margin: 0 0 1rem 0">
            <code>v{s().version}</code>
            <br />
            <span class="dim">up {Number(s().uptimeSeconds ?? 0n)}s</span>
            <br />
            <span class="dim">{s().runningTasks} running</span>
          </p>
        )}
      </Show>

      <h3 class="section-title">Models</h3>
      <Show
        when={statusQ.data && statusQ.data.models.length > 0}
        fallback={<p class="dim">none</p>}
      >
        <ul style="list-style: none; padding: 0; margin: 0 0 1rem 0; font-size: 12px">
          <For each={statusQ.data!.models}>
            {(m) => (
              <li style="display: flex; align-items: center; gap: 0.4rem; padding: 0.15rem 0">
                <span
                  class={`dot ${m.online ? (m.quarantined ? 'warn' : 'good') : 'offline'}`}
                />
                <span style="flex: 1; overflow: hidden; text-overflow: ellipsis">
                  {m.name}
                </span>
                <span class="fade" style="font-size: 10px">{m.kind}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
};

export default SidebarRight;
