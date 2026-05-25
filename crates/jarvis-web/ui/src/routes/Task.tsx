import {
  Show,
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  onMount,
  type Component,
} from 'solid-js';
import { useParams, useNavigate } from '@solidjs/router';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { taskQuery, qkTaskList } from '~/lib/api/queries';
import { useTaskEventStream } from '~/lib/api/streams';
import { jarvis } from '~/lib/api/client';
import Transcript from '~/components/Transcript';
import Timeline from '~/features/timeline/Timeline';
import DiffByIntent from '~/features/diff/DiffByIntent';
import TerminalLogs from '~/components/TerminalLogs';
import Telemetry from '~/components/Telemetry';
import AppErrorBoundary from '~/components/ErrorBoundary';
import { SkeletonList } from '~/components/Skeleton';

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
  const [activeTab, setActiveTab] = createSignal<'files' | 'terminal' | 'telemetry'>('files');
  // § C UX — goal text per task id, populated lazily from getTask so the
  // transcript can label each follow-up turn. The leaf (latest task in
  // the chain) is what new follow-ups attach to.
  const [taskGoals, setTaskGoals] = createSignal<Record<string, string>>({});

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

  // Distinct task ids present in the streamed events, oldest → newest by
  // first appearance. The last one is the chain leaf.
  const chainTaskIds = createMemo<string[]>(() => {
    const seen: string[] = [];
    for (const e of events()) {
      if (e.taskId && !seen.includes(e.taskId)) seen.push(e.taskId);
    }
    return seen;
  });

  // The leaf task — what a new follow-up should attach to so the chain
  // stays linear instead of fanning siblings off an old ancestor.
  const leafTaskId = createMemo(() => {
    const ids = chainTaskIds();
    return ids.length > 0 ? ids[ids.length - 1] : params.id;
  });

  // Lazily fetch the goal of every task in the chain for the transcript
  // turn headers. getTask is cheap + cached; fetch each id once.
  createEffect(() => {
    const ids = chainTaskIds();
    const known = taskGoals();
    for (const id of ids) {
      if (known[id] !== undefined) continue;
      jarvis
        .getTask({ id })
        .then((t) =>
          setTaskGoals((prev) => ({ ...prev, [id]: t.goal })),
        )
        .catch(() => {
          /* leave unset — transcript falls back to "follow-up" */
        });
    }
  });

  // § C UX — follow the live conversation. After submitting a follow-up,
  // the page navigates to the new (running) task and we want the user
  // looking at the BOTTOM where the new turn is unfolding, not scrolled
  // back to the top of the chain.
  //
  // The actual scroll container is `<main class="app-main">` (the layout
  // shell), NOT the window — so we target it explicitly.
  //
  // `autoFollow` stays true while the user is near the bottom; if they
  // scroll up to read earlier turns we stop yanking them back down.
  let autoFollow = true;
  const scroller = (): HTMLElement | null =>
    document.querySelector('main.app-main');
  const nearBottom = () => {
    const el = scroller();
    if (!el) return true;
    return el.scrollTop + el.clientHeight >= el.scrollHeight - 200;
  };
  const onScroll = () => {
    autoFollow = nearBottom();
  };
  onMount(() => {
    const el = scroller();
    el?.addEventListener('scroll', onScroll, { passive: true });
    onCleanup(() => el?.removeEventListener('scroll', onScroll));
  });

  const scrollToBottom = (smooth: boolean) =>
    queueMicrotask(() => {
      const el = scroller();
      if (!el) return;
      el.scrollTo({
        top: el.scrollHeight,
        behavior: smooth ? 'smooth' : 'auto',
      });
    });

  // On task load: jump to the bottom. The Task page is a conversation —
  // opening it (from a follow-up submit or a dashboard card) should land
  // on the newest turn, the way any chat UI behaves. A double rAF lets
  // the transcript + timeline finish laying out before we measure.
  createEffect(
    on([() => stream.loaded(), () => params.id], ([loaded]) => {
      if (!loaded) return;
      autoFollow = true;
      requestAnimationFrame(() =>
        requestAnimationFrame(() => scrollToBottom(false)),
      );
    }),
  );

  // While events stream in, keep following the bottom if the user hasn't
  // scrolled away.
  createEffect(
    on(
      () => events().length,
      (len, prev) => {
        if (prev !== undefined && len > prev && autoFollow) {
          scrollToBottom(true);
        }
      },
    ),
  );

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
        // § C UX — attach to the chain LEAF, not whatever task page we
        // happen to be viewing, so the conversation stays a linear
        // thread instead of fanning siblings off an ancestor.
        parentTaskId: leafTaskId(),
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

            <Show when={stream.loaded()} fallback={<SkeletonList count={4} lines={3} />}>
              <div class="task-split-container">
                {/* Left Pane: Timeline, Event Transcript, and follow-up form */}
                <div class="task-pane-left">
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
                    <Transcript
                      events={events()}
                      selectedEvtId={selectedEvtId()}
                      taskGoals={taskGoals()}
                    />
                  </Show>
                  <Show when={view() === 'diff'}>
                    <DiffByIntent taskId={params.id} />
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
                </div>

                {/* Right Pane: Premium Workspace Tabs (Files, Terminal Logs, Telemetry) */}
                <div class="task-pane-right">
                  <div class="workspace-tabs">
                    <button
                      type="button"
                      class={`workspace-tab ${activeTab() === 'files' ? 'active' : ''}`}
                      onClick={() => setActiveTab('files')}
                    >
                      📁 files & diffs
                    </button>
                    <button
                      type="button"
                      class={`workspace-tab ${activeTab() === 'terminal' ? 'active' : ''}`}
                      onClick={() => setActiveTab('terminal')}
                    >
                      💻 terminal logs
                    </button>
                    <button
                      type="button"
                      class={`workspace-tab ${activeTab() === 'telemetry' ? 'active' : ''}`}
                      onClick={() => setActiveTab('telemetry')}
                    >
                      📊 telemetry
                    </button>
                  </div>

                  <div class="workspace-content">
                    <Show when={activeTab() === 'files'}>
                      <DiffByIntent taskId={params.id} />
                    </Show>
                    <Show when={activeTab() === 'terminal'}>
                      <TerminalLogs events={events()} />
                    </Show>
                    <Show when={activeTab() === 'telemetry'}>
                      <Telemetry taskId={params.id} />
                    </Show>
                  </div>
                </div>
              </div>
            </Show>
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

const TaskRoute: Component = () => (
  <AppErrorBoundary name="Task">
    <Task />
  </AppErrorBoundary>
);

export default TaskRoute;
