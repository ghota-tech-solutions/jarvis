import { For, Show, createMemo, createSignal, type Component } from 'solid-js';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';
import type { Memory } from '~/lib/api/gen/jarvis_pb';
import { SkeletonList } from '~/components/Skeleton';
import { EmptyState } from '~/components/EmptyState';
import BulkActionsBar from '~/components/BulkActionsBar';
import { createBulkSelect } from '~/lib/bulk-select';

const STATUS_LABEL: Record<string, string> = {
  candidate: 'pending',
  active: 'promoted',
  forgotten: 'forgotten',
};

const KIND_TONE: Record<string, 'good' | 'warn' | 'accent' | ''> = {
  pattern: 'accent',
  preference: 'warn',
  fact: 'good',
};

const MemoryRow: Component<{ m: Memory; onChange: () => void }> = (p) => {
  const qc = useQueryClient();
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal(p.m.text);
  const [busy, setBusy] = createSignal(false);

  const refresh = async () => {
    await qc.invalidateQueries({ queryKey: ['memories'] });
    p.onChange();
  };

  const promote = async () => {
    setBusy(true);
    try {
      await jarvis.promoteMemory({ id: p.m.id, text: '' });
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  const forget = async () => {
    setBusy(true);
    try {
      await jarvis.forgetMemory({ id: p.m.id });
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  const saveEdit = async () => {
    setBusy(true);
    try {
      await jarvis.editMemory({
        id: p.m.id,
        text: draft(),
        scope: p.m.scope,
        scopeValue: p.m.scopeValue,
        kind: p.m.kind,
      });
      setEditing(false);
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  return (
    <li class="memory-row">
      <div class="memory-meta">
        <span class={`pill ${KIND_TONE[p.m.kind] ?? ''}`}>{p.m.kind}</span>
        <span class="dim" style="font-size: 11px">
          {p.m.scope}
          {p.m.scope === 'workdir' && p.m.scopeValue && `: ${shortenPath(p.m.scopeValue)}`}
        </span>
        <span
          class="pill"
          style={`margin-left: auto; ${
            p.m.status === 'active' ? 'color: var(--good)' : 'color: var(--dim)'
          }`}
        >
          {STATUS_LABEL[p.m.status] ?? p.m.status}
        </span>
        <Show when={p.m.usageCount > 0}>
          <span class="fade" style="font-size: 11px">
            used {p.m.usageCount}×
          </span>
        </Show>
      </div>
      <Show
        when={editing()}
        fallback={<p class="memory-text">{p.m.text}</p>}
      >
        <textarea
          class="textarea"
          value={draft()}
          onInput={(e) => setDraft(e.currentTarget.value)}
          rows={3}
        />
      </Show>
      <div class="memory-actions">
        <Show when={!editing()}>
          <Show when={p.m.status === 'candidate'}>
            <button class="btn" disabled={busy()} onClick={promote}>
              promote
            </button>
          </Show>
          <Show when={p.m.status === 'active'}>
            <button class="btn ghost" disabled={busy()} onClick={forget}>
              forget
            </button>
          </Show>
          <Show when={p.m.status === 'candidate'}>
            <button class="btn ghost" disabled={busy()} onClick={forget}>
              dismiss
            </button>
          </Show>
          <button class="btn ghost" onClick={() => setEditing(true)}>
            edit
          </button>
        </Show>
        <Show when={editing()}>
          <button class="btn" disabled={busy()} onClick={saveEdit}>
            save
          </button>
          <button
            class="btn ghost"
            onClick={() => {
              setEditing(false);
              setDraft(p.m.text);
            }}
          >
            cancel
          </button>
        </Show>
      </div>
    </li>
  );
};

const MemoryList: Component<{ workdirFilter?: string }> = (p) => {
  const q = createQuery(() => ({
    queryKey: ['memories', p.workdirFilter ?? null],
    queryFn: async () => {
      return await jarvis.listMemories({
        scope: '',
        scopeValue: p.workdirFilter ?? '',
        status: '',
        limit: 200,
      });
    },
    refetchInterval: 5000,
  }));

  const grouped = createMemo(() => {
    const list = q.data?.memories ?? [];
    return {
      candidate: list.filter((m) => m.status === 'candidate'),
      active: list.filter((m) => m.status === 'active'),
    };
  });

  // § F1.7 — bulk select on the Candidates section. The most operationally
  // painful case is reviewing N proposals one-by-one after a long task.
  const bulk = createBulkSelect<bigint>();
  const [bulkBusy, setBulkBusy] = createSignal(false);
  const runBulk = async (op: 'promote' | 'forget') => {
    const ids = bulk.ids();
    if (ids.length === 0) return;
    setBulkBusy(true);
    try {
      for (const id of ids) {
        if (op === 'promote') {
          await jarvis.promoteMemory({ id, text: '' });
        } else {
          await jarvis.forgetMemory({ id });
        }
      }
      bulk.clear();
      await q.refetch();
    } finally {
      setBulkBusy(false);
    }
  };

  return (
    <section class="memory-page">
      <header style="margin-bottom: 1rem">
        <h2 class="heading" style="margin: 0">Memory</h2>
        <p class="dim" style="font-size: 12px; margin: 0">
          The agent's learned constraints. Promote the ones you want injected
          into future tasks' system prompts.
        </p>
      </header>

      <Show
        when={q.isPending && !q.data}
        fallback={
          <>
            <h3 class="section-title">Candidates · {grouped().candidate.length}</h3>
            <Show
              when={grouped().candidate.length > 0}
              fallback={<p class="dim">none — finish a task to see proposals</p>}
            >
              <BulkActionsBar
                count={bulk.count()}
                onClear={() => bulk.clear()}
                busy={bulkBusy()}
                actions={[
                  {
                    label: bulkBusy() ? 'Promoting…' : 'Promote selected',
                    variant: 'primary',
                    onClick: () => runBulk('promote'),
                  },
                  {
                    label: bulkBusy() ? 'Forgetting…' : 'Forget selected',
                    variant: 'danger',
                    onClick: () => runBulk('forget'),
                    confirm: 'Forget the selected candidate memories?',
                  },
                ]}
              />
              <div style="margin-bottom: 0.4rem; font-size: 11px">
                <button
                  type="button"
                  class="btn ghost"
                  style="padding: 0.15rem 0.4rem"
                  onClick={() =>
                    bulk.count() === grouped().candidate.length
                      ? bulk.clear()
                      : bulk.selectAll(grouped().candidate.map((m) => m.id))
                  }
                >
                  {bulk.count() === grouped().candidate.length
                    ? 'Select none'
                    : 'Select all'}
                </button>
              </div>
              <ul class="memory-list">
                <For each={grouped().candidate}>
                  {(m) => (
                    <li class="bulk-row" style="list-style: none">
                      <input
                        type="checkbox"
                        aria-label={`Select memory ${m.text.slice(0, 40)}`}
                        checked={bulk.isSelected(m.id)}
                        onChange={() => bulk.toggle(m.id)}
                      />
                      <div class="bulk-row-content">
                        <MemoryRow m={m} onChange={() => q.refetch()} />
                      </div>
                    </li>
                  )}
                </For>
              </ul>
            </Show>

            <h3 class="section-title" style="margin-top: 1.5rem">
              Active · {grouped().active.length}
            </h3>
            <Show
              when={grouped().active.length > 0}
              fallback={
                <EmptyState
                  title="No active memories"
                  hint="When jarvis completes a task, it proposes patterns and facts it learned. Promote them here to keep them in mind for future tasks."
                />
              }
            >
              <ul class="memory-list">
                <For each={grouped().active}>
                  {(m) => <MemoryRow m={m} onChange={() => q.refetch()} />}
                </For>
              </ul>
            </Show>
          </>
        }
      >
        <SkeletonList count={4} lines={2} />
      </Show>
    </section>
  );
};

function shortenPath(p: string): string {
  if (p.length < 40) return p;
  const parts = p.replace(/\\/g, '/').split('/');
  if (parts.length <= 3) return p;
  return `…/${parts.slice(-3).join('/')}`;
}

export default MemoryList;
