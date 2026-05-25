import {
  For,
  Show,
  createMemo,
  createSignal,
  type Component,
} from 'solid-js';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';
import type { Schedule } from '~/lib/api/gen/jarvis_pb';
import AppErrorBoundary from '~/components/ErrorBoundary';

const fmtNext = (micros: bigint): string => {
  const n = Number(micros);
  if (!n) return '—';
  const d = new Date(n / 1000);
  const dt = d.getTime() - Date.now();
  if (dt < 0) return d.toLocaleString();
  const secs = Math.round(dt / 1000);
  if (secs < 60) return `in ${secs}s · ${d.toLocaleTimeString()}`;
  if (secs < 3600) return `in ${Math.round(secs / 60)}m · ${d.toLocaleTimeString()}`;
  return `in ${Math.round(secs / 3600)}h · ${d.toLocaleString()}`;
};

const fmtLast = (micros: bigint, taskId: string): string => {
  const n = Number(micros);
  if (!n) return 'never';
  const ago = Math.round((Date.now() - n / 1000) / 1000);
  const when = ago < 60 ? `${ago}s ago` : ago < 3600 ? `${Math.round(ago / 60)}m ago` : `${Math.round(ago / 3600)}h ago`;
  return taskId ? `${when} → ${taskId.slice(0, 8)}` : when;
};

const NewScheduleForm: Component<{ onCreated: () => void }> = (p) => {
  const [label, setLabel] = createSignal('');
  const [cron, setCron] = createSignal('0 */2 * * *');
  const [goal, setGoal] = createSignal('');
  const [workdir, setWorkdir] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const onSubmit = async (ev: Event) => {
    ev.preventDefault();
    if (!cron().trim() || !goal().trim() || busy()) return;
    setBusy(true);
    setError(null);
    try {
      await jarvis.createSchedule({
        id: '',
        cron: cron().trim(),
        goal: goal().trim(),
        workdir: workdir().trim(),
        sandbox: '',
        netPolicy: '',
        routingPolicy: '',
        maxSteps: 0,
        label: label().trim(),
        paused: false,
      });
      setLabel('');
      setGoal('');
      setWorkdir('');
      p.onCreated();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="form" onSubmit={onSubmit}>
      <h3 class="section-title" style="margin: 0">New schedule</h3>
      <div class="row">
        <input
          class="input"
          placeholder="label (optional)"
          value={label()}
          onInput={(e) => setLabel(e.currentTarget.value)}
          style="flex: 1 1 160px"
        />
        <input
          class="input"
          placeholder="cron — e.g. */30 * * * * or 0 */2 * * *"
          value={cron()}
          onInput={(e) => setCron(e.currentTarget.value)}
          style="flex: 1 1 240px; font-family: ui-monospace, monospace"
        />
      </div>
      <textarea
        class="textarea"
        placeholder="Goal — what should the agent do on each run?"
        value={goal()}
        onInput={(e) => setGoal(e.currentTarget.value)}
        rows={2}
      />
      <div class="row">
        <input
          class="input"
          placeholder="workdir (empty = daemon cwd)"
          value={workdir()}
          onInput={(e) => setWorkdir(e.currentTarget.value)}
          style="flex: 1 1 240px"
        />
        <button
          type="submit"
          class="btn"
          disabled={busy() || !cron().trim() || !goal().trim()}
        >
          {busy() ? 'creating…' : 'create'}
        </button>
      </div>
      <Show when={error()}>
        <p class="error" style="font-size: 12px; margin: 0">{error()}</p>
      </Show>
    </form>
  );
};

const ScheduleRow: Component<{ s: Schedule; onChange: () => void }> = (p) => {
  const [busy, setBusy] = createSignal<'run' | 'delete' | null>(null);
  const [err, setErr] = createSignal<string | null>(null);

  const spec = () => p.s.spec!;

  const runNow = async () => {
    setBusy('run');
    setErr(null);
    try {
      await jarvis.runScheduleNow({ id: spec().id });
      p.onChange();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const remove = async () => {
    if (!confirm(`Delete schedule "${spec().label || spec().goal.slice(0, 40)}"?`)) return;
    setBusy('delete');
    setErr(null);
    try {
      await jarvis.deleteSchedule({ id: spec().id });
      p.onChange();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <li class="schedule-row">
      <div class="schedule-head">
        <Show when={spec().label}>
          <span class="heading" style="font-size: 13px">{spec().label}</span>
        </Show>
        <code class="dim" style="font-size: 11px">{spec().cron}</code>
        <Show when={spec().paused}>
          <span class="pill warn">paused</span>
        </Show>
        <span class="dim" style="margin-left: auto; font-size: 11px">
          {spec().id.slice(0, 8)}
        </span>
      </div>
      <p class="schedule-goal">{spec().goal}</p>
      <div class="schedule-meta">
        <span class="dim">next:</span>
        <span class={spec().paused ? 'fade' : 'warn'}>{fmtNext(p.s.nextRunMicros)}</span>
        <span class="fade">·</span>
        <span class="dim">last:</span>
        <span>{fmtLast(p.s.lastRunMicros, p.s.lastTaskId)}</span>
        <Show when={spec().workdir}>
          <span class="fade" style="margin-left: auto">{spec().workdir}</span>
        </Show>
      </div>
      <div class="schedule-actions">
        <button class="btn" disabled={busy() !== null} onClick={runNow}>
          {busy() === 'run' ? 'firing…' : 'run now'}
        </button>
        <button class="btn ghost" disabled={busy() !== null} onClick={remove}>
          {busy() === 'delete' ? 'deleting…' : 'delete'}
        </button>
      </div>
      <Show when={err()}>
        <p class="error" style="font-size: 12px">{err()}</p>
      </Show>
    </li>
  );
};

const Schedules: Component = () => {
  const qc = useQueryClient();
  const q = createQuery(() => ({
    queryKey: ['schedules'],
    queryFn: async () => await jarvis.listSchedules({}),
    refetchInterval: 5000,
  }));

  const refresh = () => qc.invalidateQueries({ queryKey: ['schedules'] });
  const list = createMemo(() => q.data?.schedules ?? []);

  return (
    <section class="schedules-page">
      <header style="margin-bottom: 1rem">
        <h2 class="heading" style="margin: 0">Schedules</h2>
        <p class="dim" style="font-size: 12px; margin: 0">
          Cron-driven autonomous runs. Edit/pause via the daemon's jarvis.toml,
          or just delete + recreate from here.
        </p>
      </header>

      <NewScheduleForm onCreated={refresh} />

      <h3 class="section-title" style="margin-top: 1.5rem">
        Active · {list().length}
      </h3>
      <Show
        when={list().length > 0}
        fallback={<p class="dim">no schedules yet — create one above</p>}
      >
        <ul class="schedule-list">
          <For each={list()}>{(s) => <ScheduleRow s={s} onChange={refresh} />}</For>
        </ul>
      </Show>
    </section>
  );
};

const SchedulesRoute: Component = () => (
  <AppErrorBoundary name="Schedules">
    <Schedules />
  </AppErrorBoundary>
);

export default SchedulesRoute;
