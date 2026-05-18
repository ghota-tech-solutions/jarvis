import { Show, createSignal, type Component } from 'solid-js';
import { useParams, useNavigate } from '@solidjs/router';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { taskQuery, timelineQuery, qkTaskList } from '~/lib/api/queries';
import { jarvis } from '~/lib/api/client';
import Transcript from '~/components/Transcript';

const Task: Component = () => {
  const params = useParams<{ id: string }>();
  const nav = useNavigate();
  const qc = useQueryClient();
  const taskQ = createQuery(() => taskQuery(params.id));
  const timelineQ = createQuery(() => timelineQuery(params.id));

  const [followup, setFollowup] = createSignal('');
  const [submitting, setSubmitting] = createSignal(false);

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

            <Show when={timelineQ.data} fallback={<p class="dim">loading events…</p>}>
              <Transcript events={timelineQ.data!.events} />
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
