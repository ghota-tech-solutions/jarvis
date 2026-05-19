import { Show, createMemo, createSignal, type Component } from 'solid-js';
import { useParams, useNavigate } from '@solidjs/router';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { taskQuery, qkTaskList } from '~/lib/api/queries';
import { useTaskEventStream } from '~/lib/api/streams';
import { jarvis } from '~/lib/api/client';
import Transcript from '~/components/Transcript';
import Timeline from '~/features/timeline/Timeline';
import DiffByIntent from '~/features/diff/DiffByIntent';

const Task: Component = () => {
  const params = useParams<{ id: string }>();
  const nav = useNavigate();
  const qc = useQueryClient();
  const taskQ = createQuery(() => taskQuery(params.id));
  // § C.M-D — live event stream replaces the 3-second polling. The
  // shape matches what timelineQuery used to expose; only the source
  // changed (initial snapshot + Connect server-streaming tail).
  const stream = useTaskEventStream(() => params.id);

  const [followup, setFollowup] = createSignal('');
  const [submitting, setSubmitting] = createSignal(false);
  const [view, setView] = createSignal<'timeline' | 'transcript' | 'both' | 'diff'>('both');
  const [selectedEvtId, setSelectedEvtId] = createSignal(0);

  const scrollToEvent = (id: number) => {
    setSelectedEvtId(id);
    if (id === 0) return;
    queueMicrotask(() => {
      const el = document.querySelector(`[data-evt-id="${id}"]`);
      el?.scrollIntoView({ block: 'center', behavior: 'smooth' });
    });
  };

  const events = createMemo(() => stream.events());
  const spans = createMemo(() => stream.spans());
  const minTs = createMemo(() => stream.minTsMicros());
  const maxTs = createMemo(() => stream.maxTsMicros());

  const onContinue = async (ev: Event) => {
    ev.preventDefault();
    if (!followup().trim() || submitting()) return;
    setSubmitting(true);
    try {
      const handle = await jarvis.submitTask({
        goal: followup(),
        workdir: '',
        sandbox: '',
        netPolicy: '',
        routingPolicy: '',
        useWorktree: false,
        maxSteps: 0,
        baseRef: '',
        requireCaps: [],
        parentTaskId: params.id,
      });
      setFollowup('');
      await qc.invalidateQueries({ queryKey: qkTaskList(true) });
      nav(`/task/${handle.id}`);
    } finally {
      setSubmitting(false);
    }
  };

  const onCancel = async () => {
    if (!confirm(`Cancel task ${params.id.slice(0, 8)}?`)) return;
    await jarvis.cancelTask({ id: params.id });
    await qc.invalidateQueries({ queryKey: qkTaskList(true) });
  };

  return (
    <section class="task-page">
      <Show
        when={taskQ.data}
        fallback={
          <p class="dim">{taskQ.error ? `error: ${taskQ.error}` : 'loading…'}</p>
        }
      >
        {(t) => (
          <>
            <header style="margin-bottom: 1rem">
              <h2 class="heading" style="margin: 0">
                {t().goal}{' '}
                <span class={`pill ${statusClass(t().status)}`}>{t().status}</span>
              </h2>
              <p class="dim" style="margin: 0.25rem 0; font-size: 12px">
                <code>{t().id.slice(0, 8)}</code>
                <span class="fade"> · </span>
                {t().sandbox || 'native'}
                <Show when={t().netPolicy}>
                  <span class="fade"> / </span>
                  {t().netPolicy}
                </Show>
                <Show when={t().worktreeBranch}>
                  <span class="fade"> · </span>
                  branch <code>{t().worktreeBranch}</code>
                </Show>
              </p>
            </header>

            <Show when={stream.loaded()} fallback={<p class="dim">loading events…</p>}>
              <div class="view-toggle">
                <button
                  type="button"
                  class={`btn ghost ${view() === 'both' ? 'active' : ''}`}
                  onClick={() => setView('both')}
                >
                  both
                </button>
                <button
                  type="button"
                  class={`btn ghost ${view() === 'timeline' ? 'active' : ''}`}
                  onClick={() => setView('timeline')}
                >
                  timeline
                </button>
                <button
                  type="button"
                  class={`btn ghost ${view() === 'transcript' ? 'active' : ''}`}
                  onClick={() => setView('transcript')}
                >
                  transcript
                </button>
                <button
                  type="button"
                  class={`btn ghost ${view() === 'diff' ? 'active' : ''}`}
                  onClick={() => setView('diff')}
                >
                  diff
                </button>
              </div>
              <Show when={view() === 'both' || view() === 'timeline'}>
                <Timeline
                  events={events()}
                  spans={spans()}
                  minTs={minTs()}
                  maxTs={maxTs()}
                  selectedEvtId={selectedEvtId()}
                  onSelect={scrollToEvent}
                />
              </Show>
              <Show when={view() === 'both' || view() === 'transcript'}>
                <Transcript events={events()} selectedEvtId={selectedEvtId()} />
              </Show>
              <Show when={view() === 'diff'}>
                <DiffByIntent taskId={params.id} />
              </Show>
            </Show>

            <form class="form" onSubmit={onContinue} style="margin-top: 1.5rem">
              <textarea
                class="textarea"
                placeholder="Ask for a follow-up change — Enter sends, Shift+Enter newlines"
                value={followup()}
                onInput={(e) => setFollowup(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault();
                    onContinue(e);
                  }
                }}
              />
              <div class="row" style="justify-content: flex-end; gap: 0.5rem">
                <button
                  type="button"
                  class="btn ghost"
                  onClick={onCancel}
                  disabled={t().status !== 'running' && t().status !== 'pending'}
                  title="Cancel current task"
                >
                  cancel
                </button>
                <button
                  type="submit"
                  class="btn"
                  disabled={submitting() || !followup().trim()}
                >
                  {submitting() ? 'sending…' : 'send'}
                </button>
              </div>
            </form>
          </>
        )}
      </Show>
    </section>
  );
};

const statusClass = (s: string): 'good' | 'warn' | 'error' | '' => {
  switch (s) {
    case 'completed': return 'good';
    case 'running':
    case 'pending':
      return 'warn';
    case 'failed':
    case 'cancelled':
      return 'error';
    default: return '';
  }
};

export default Task;
