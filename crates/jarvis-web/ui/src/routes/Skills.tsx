// § F2.3 — Skills curation page.
//
// Reads `ListSkills` for the user-selected workdir and renders three
// columns (candidates / active / forgotten) with the markdown body
// rendered inline + Promote / Forget actions. The workdir selector
// reuses the tasks list so the user can pick any workdir the daemon
// has seen at least one task for; defaults to the most recently
// active.
//
// Skills are filesystem-backed (`<workdir>/.jarvis/skills/`); these
// RPCs are the in-browser shortcut to the `mv` flow.

import { For, Show, createMemo, createSignal, type Component } from 'solid-js';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';
import type { Skill } from '~/lib/api/gen/jarvis_pb';
import AppErrorBoundary from '~/components/ErrorBoundary';
import { EmptyState } from '~/components/EmptyState';
import Markdown from '~/components/Markdown';

type Status = 'candidate' | 'active' | 'forgotten';

const SkillCard: Component<{
  s: Skill;
  status: Status;
  onPromote: () => void;
  onForget: () => void;
  busy: boolean;
}> = (p) => {
  const [expanded, setExpanded] = createSignal(false);
  return (
    <article class={`skill-card status-${p.status}`}>
      <header class="skill-card-header">
        <span class="skill-card-name">{p.s.name}</span>
        <button
          type="button"
          class="btn ghost skill-card-toggle"
          onClick={() => setExpanded((v) => !v)}
          title={expanded() ? 'Collapse body' : 'Show body'}
        >
          {expanded() ? '▾' : '▸'}
        </button>
      </header>
      <p class="skill-card-title">{p.s.title}</p>
      <Show when={p.s.trigger}>
        <p class="skill-card-trigger">
          <span class="dim">when:</span> {p.s.trigger}
        </p>
      </Show>
      <Show when={expanded()}>
        <div class="skill-card-body">
          <Markdown text={p.s.body} />
        </div>
      </Show>
      <footer class="skill-card-actions">
        <Show when={p.status !== 'active'}>
          <button
            type="button"
            class="btn"
            disabled={p.busy}
            onClick={() => p.onPromote()}
          >
            {p.busy ? '…' : 'Promote'}
          </button>
        </Show>
        <Show when={p.status !== 'forgotten'}>
          <button
            type="button"
            class="btn ghost"
            disabled={p.busy}
            onClick={() => p.onForget()}
          >
            Forget
          </button>
        </Show>
        <span class="dim skill-card-path" title={p.s.path}>
          {p.s.path}
        </span>
      </footer>
    </article>
  );
};

const SkillColumn: Component<{
  title: string;
  skills: Skill[];
  status: Status;
  onPromote: (name: string) => void;
  onForget: (name: string) => void;
  busy: string | null;
}> = (p) => {
  return (
    <section class="skill-column">
      <header class="skill-column-header">
        <h3>{p.title}</h3>
        <span class="dim">· {p.skills.length}</span>
      </header>
      <Show
        when={p.skills.length > 0}
        fallback={<p class="dim skill-column-empty">none</p>}
      >
        <div class="skill-column-list">
          <For each={p.skills}>
            {(s) => (
              <SkillCard
                s={s}
                status={p.status}
                onPromote={() => p.onPromote(s.name)}
                onForget={() => p.onForget(s.name)}
                busy={p.busy === s.name}
              />
            )}
          </For>
        </div>
      </Show>
    </section>
  );
};

const Skills: Component = () => {
  const qc = useQueryClient();
  const [workdir, setWorkdir] = createSignal('');
  const [busy, setBusy] = createSignal<string | null>(null);

  // Use any task's workdir as a hint for the selector. The query is
  // shared with Dashboard so we get the cached value for free.
  const tasksQ = createQuery(() => ({
    queryKey: ['tasks-all'],
    queryFn: async () => await jarvis.listTasks({ includeFinished: true }),
    staleTime: 30_000,
  }));

  const workdirs = createMemo(() => {
    const seen = new Set<string>();
    for (const t of tasksQ.data?.tasks ?? []) {
      if (t.workdir) seen.add(t.workdir);
    }
    return Array.from(seen).sort();
  });

  // Auto-pick the first workdir once tasks load.
  createMemo(() => {
    if (!workdir() && workdirs().length > 0) {
      setWorkdir(workdirs()[0]);
    }
  });

  const skillsQ = createQuery(() => ({
    queryKey: ['skills', workdir()],
    queryFn: async () => await jarvis.listSkills({ workdir: workdir() }),
    enabled: !!workdir(),
    refetchInterval: 10_000,
  }));

  const promote = async (name: string) => {
    if (busy()) return;
    setBusy(name);
    try {
      await jarvis.promoteSkill({ workdir: workdir(), name });
      await qc.invalidateQueries({ queryKey: ['skills', workdir()] });
    } finally {
      setBusy(null);
    }
  };
  const forget = async (name: string) => {
    if (busy()) return;
    if (!window.confirm(`Move skill "${name}" to forgotten/?`)) return;
    setBusy(name);
    try {
      await jarvis.forgetSkill({ workdir: workdir(), name });
      await qc.invalidateQueries({ queryKey: ['skills', workdir()] });
    } finally {
      setBusy(null);
    }
  };

  return (
    <AppErrorBoundary name="Skills">
      <section class="skills-page">
        <header class="skills-header">
          <h2 class="heading" style="margin: 0">
            Skills
          </h2>
          <p class="dim" style="margin: 0.2rem 0 0 0; font-size: 12px">
            Self-authored procedures the agent distilled from past tasks.
            Promote candidates you want injected into future task prompts.
            Files live under <code>&lt;workdir&gt;/.jarvis/skills/</code>.
          </p>
        </header>

        <Show
          when={workdirs().length > 0}
          fallback={
            <EmptyState
              title="No workdirs yet"
              hint="Submit a task first — once it completes, the post-verdict skill extractor may propose a candidate here."
            />
          }
        >
          <div class="skills-toolbar">
            <label class="dim" style="font-size: 11px">
              workdir
            </label>
            <select
              class="select"
              style="max-width: 420px"
              value={workdir()}
              onChange={(e) => setWorkdir(e.currentTarget.value)}
            >
              <For each={workdirs()}>
                {(w) => <option value={w}>{w}</option>}
              </For>
            </select>
          </div>

          <Show
            when={skillsQ.data}
            fallback={
              <p class="dim" style="font-size: 12px; padding: 1rem">
                loading skills…
              </p>
            }
          >
            <Show
              when={
                (skillsQ.data!.candidates.length ?? 0) +
                  (skillsQ.data!.active.length ?? 0) +
                  (skillsQ.data!.forgotten.length ?? 0) >
                0
              }
              fallback={
                <EmptyState
                  title="No skills yet"
                  hint="The agent writes a skill candidate here after a successful task that involved 3+ tool calls. Try a non-trivial task in this workdir."
                />
              }
            >
              <div class="skills-grid">
                <SkillColumn
                  title="Candidates"
                  skills={skillsQ.data!.candidates}
                  status="candidate"
                  onPromote={promote}
                  onForget={forget}
                  busy={busy()}
                />
                <SkillColumn
                  title="Active"
                  skills={skillsQ.data!.active}
                  status="active"
                  onPromote={promote}
                  onForget={forget}
                  busy={busy()}
                />
                <SkillColumn
                  title="Forgotten"
                  skills={skillsQ.data!.forgotten}
                  status="forgotten"
                  onPromote={promote}
                  onForget={forget}
                  busy={busy()}
                />
              </div>
            </Show>
          </Show>
        </Show>
      </section>
    </AppErrorBoundary>
  );
};

export default Skills;
