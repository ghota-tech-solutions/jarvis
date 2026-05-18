import { For, Show, createMemo, createSignal, type Component } from 'solid-js';
import { A } from '@solidjs/router';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { taskListQuery, qkTaskList } from '~/lib/api/queries';
import { jarvis } from '~/lib/api/client';
import type { Task } from '~/lib/api/gen/jarvis_pb';

type Conversation = {
  root: Task;
  children: Task[];
};

const statusClass = (s: string): 'good' | 'warn' | 'error' | '' => {
  switch (s) {
    case 'completed':
      return 'good';
    case 'running':
    case 'pending':
      return 'warn';
    case 'failed':
    case 'cancelled':
      return 'error';
    default:
      return '';
  }
};

const shortId = (id: string) => id.split('-')[0] ?? id;

const TaskCard: Component<{ conv: Conversation }> = (p) => {
  const { root, children } = p.conv;
  return (
    <A href={`/task/${root.id}`} class="task-card">
      <div class="row">
        <span class={`pill ${statusClass(root.status)}`}>{root.status}</span>
        <code class="dim" style="font-size: 11px">{shortId(root.id)}</code>
        <Show when={children.length > 0}>
          <span class="pill accent">×{children.length + 1}</span>
        </Show>
        <span class="dim" style="margin-left: auto; font-size: 11px">
          {root.sandbox || 'native'}
        </span>
      </div>
      <div class="goal">{root.goal}</div>
    </A>
  );
};

const NewTaskForm: Component = () => {
  const qc = useQueryClient();
  const [goal, setGoal] = createSignal('');
  const [workdir, setWorkdir] = createSignal('');
  const [sandbox, setSandbox] = createSignal('');
  const [netPolicy, setNetPolicy] = createSignal('');
  const [routing, setRouting] = createSignal('');
  const [worktree, setWorktree] = createSignal(false);
  const [submitting, setSubmitting] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const onSubmit = async (ev: Event) => {
    ev.preventDefault();
    if (!goal().trim() || submitting()) return;
    setSubmitting(true);
    setError(null);
    try {
      await jarvis.submitTask({
        goal: goal(),
        workdir: workdir(),
        sandbox: sandbox(),
        netPolicy: netPolicy(),
        routingPolicy: routing(),
        useWorktree: worktree(),
        maxSteps: 0,
        baseRef: '',
        requireCaps: [],
        parentTaskId: '',
      });
      setGoal('');
      await qc.invalidateQueries({ queryKey: qkTaskList(true) });
    } catch (err) {
      setError(String(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <form class="form" onSubmit={onSubmit}>
      <h3 class="section-title" style="margin: 0">New task</h3>
      <textarea
        class="textarea"
        placeholder="Describe the goal — Enter submits, Shift+Enter newlines"
        value={goal()}
        onInput={(e) => setGoal(e.currentTarget.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            onSubmit(e);
          }
        }}
      />
      <div class="row">
        <input
          class="input"
          placeholder="workdir (empty = daemon cwd)"
          value={workdir()}
          onInput={(e) => setWorkdir(e.currentTarget.value)}
          style="flex: 1 1 220px"
        />
        <select class="select" value={sandbox()} onChange={(e) => setSandbox(e.currentTarget.value)}>
          <option value="">sandbox: default</option>
          <option value="native">native</option>
          <option value="docker">docker</option>
        </select>
        <select class="select" value={netPolicy()} onChange={(e) => setNetPolicy(e.currentTarget.value)}>
          <option value="">net: default</option>
          <option value="none">none</option>
          <option value="egress_only">egress_only</option>
          <option value="full">full</option>
        </select>
        <select class="select" value={routing()} onChange={(e) => setRouting(e.currentTarget.value)}>
          <option value="">routing: default</option>
          <option value="auto">auto</option>
          <option value="local_only">local_only</option>
          <option value="remote_only">remote_only</option>
        </select>
        <label
          style="display: flex; align-items: center; gap: 0.3rem; font-size: 12px; padding: 0 0.4rem"
          title="If workdir is a git repo, create a fresh worktree"
        >
          <input
            type="checkbox"
            checked={worktree()}
            onChange={(e) => setWorktree(e.currentTarget.checked)}
          />
          worktree
        </label>
        <button type="submit" class="btn" disabled={submitting() || !goal().trim()}>
          {submitting() ? 'submitting…' : 'submit'}
        </button>
      </div>
      <Show when={error()}>
        <div class="error" style="font-size: 12px">{error()}</div>
      </Show>
    </form>
  );
};

const Dashboard: Component = () => {
  const tasksQ = createQuery(() => taskListQuery(true));

  const conversations = createMemo<Conversation[]>(() => {
    const tasks = tasksQ.data?.tasks ?? [];
    const byId = new Map(tasks.map((t) => [t.id, t]));
    const roots: Conversation[] = [];
    const byRoot = new Map<string, Conversation>();
    for (const t of tasks) {
      const rootId = t.parentTaskId
        ? rootOf(t, byId)
        : t.id;
      let conv = byRoot.get(rootId);
      if (!conv) {
        const root = byId.get(rootId) ?? t;
        conv = { root, children: [] };
        byRoot.set(rootId, conv);
        roots.push(conv);
      }
      if (t.id !== rootId) conv.children.push(t);
    }
    // Order: active first (running/pending), then by created_at desc
    return roots.sort((a, b) => {
      const aActive = a.root.status === 'running' || a.root.status === 'pending' ? 0 : 1;
      const bActive = b.root.status === 'running' || b.root.status === 'pending' ? 0 : 1;
      if (aActive !== bActive) return aActive - bActive;
      return Number(b.root.createdAt - a.root.createdAt);
    });
  });

  return (
    <section>
      <header style="margin-bottom: 1rem">
        <h2 class="heading" style="margin: 0 0 0.2rem 0">Tasks</h2>
        <p class="dim" style="margin: 0; font-size: 12px">
          {tasksQ.data?.tasks.length ?? 0} task(s) ·{' '}
          <Show when={tasksQ.isFetching}>refreshing</Show>
        </p>
      </header>

      <div
        style="display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 0.7rem"
      >
        <For each={conversations()}>{(conv) => <TaskCard conv={conv} />}</For>
      </div>

      <NewTaskForm />
    </section>
  );
};

function rootOf(t: Task, byId: Map<string, Task>): string {
  let cur: Task | undefined = t;
  // Walk up at most 64 levels (matches ledger guard).
  for (let i = 0; i < 64 && cur; i++) {
    if (!cur.parentTaskId) return cur.id;
    cur = byId.get(cur.parentTaskId);
  }
  return t.id;
}

export default Dashboard;
