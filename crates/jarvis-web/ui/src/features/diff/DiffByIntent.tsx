import { For, Show, createMemo, type Component } from 'solid-js';
import { createQuery } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';
import PhaseGroup from './PhaseGroup';

type Props = { taskId: string };

const DiffByIntent: Component<Props> = (p) => {
  const q = createQuery(() => ({
    queryKey: ['diff', p.taskId],
    queryFn: async () => await jarvis.groupDiffByIntent({ id: p.taskId }),
    refetchInterval: 4000,
  }));

  const refresh = () => q.refetch();

  const orphans = createMemo(() => q.data?.orphanPaths ?? []);

  return (
    <section class="diff-page">
      <header style="display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.8rem">
        <h3 class="heading" style="margin: 0">Diff by intent</h3>
        <Show when={q.isFetching}>
          <span class="fade" style="font-size: 11px">refreshing…</span>
        </Show>
        <span class="dim" style="margin-left: auto; font-size: 12px">
          {(q.data?.groups.length ?? 0)} phase(s){' '}
          {orphans().length > 0 && `· ${orphans().length} orphan`}
        </span>
      </header>
      <Show when={q.data} fallback={<p class="dim">loading…</p>}>
        <Show
          when={(q.data?.groups.length ?? 0) > 0}
          fallback={<p class="dim">no diffs yet — agent has not written files</p>}
        >
          <For each={q.data!.groups}>
            {(g) => <PhaseGroup taskId={p.taskId} group={g} onChange={refresh} />}
          </For>
        </Show>
        <Show when={orphans().length > 0}>
          <details class="orphan-files">
            <summary class="dim">
              {orphans().length} orphan path(s) — written without a discoverable
              parent decision
            </summary>
            <ul>
              <For each={orphans()}>{(p) => <li><code>{p}</code></li>}</For>
            </ul>
          </details>
        </Show>
      </Show>
    </section>
  );
};

export default DiffByIntent;
