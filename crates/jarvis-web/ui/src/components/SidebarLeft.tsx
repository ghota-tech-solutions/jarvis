import { For, Show, createMemo, type Component } from 'solid-js';
import { A } from '@solidjs/router';
import { createQuery } from '@tanstack/solid-query';
import { taskListQuery } from '~/lib/api/queries';
import type { Task } from '~/lib/api/gen/jarvis_pb';
import { useT } from '~/lib/i18n';

type Project = {
  workdir: string;
  tasks: Task[];
};

const folderName = (path: string): string => {
  if (!path) return '·';
  const norm = path.replace(/\\/g, '/').replace(/\/$/, '');
  return norm.split('/').pop() || norm;
};

const SidebarLeft: Component = () => {
  const t = useT();
  const tasksQ = createQuery(() => taskListQuery(true));

  const projects = createMemo<Project[]>(() => {
    const tasks = tasksQ.data?.tasks ?? [];
    const byPath = new Map<string, Task[]>();
    for (const t of tasks) {
      const arr = byPath.get(t.workdir) ?? [];
      arr.push(t);
      byPath.set(t.workdir, arr);
    }
    return Array.from(byPath.entries())
      .sort((a, b) => a[0].localeCompare(b[0]))
      .map(([workdir, tasks]) => ({ workdir, tasks }));
  });

  return (
    <div>
      <h3 class="section-title">Navigation</h3>
      <div style="display: flex; flex-direction: column; gap: 0.2rem; margin-bottom: 1.2rem">
        <A href="/" class="nav-link" activeClass="nav-active" end>
          ⌂ {t().nav.dashboard}
        </A>
        <A href="/fleet" class="nav-link" activeClass="nav-active">
          ⌹ {t().nav.fleet}
        </A>
        <A href="/memory" class="nav-link" activeClass="nav-active">
          ⌥ {t().nav.memory}
        </A>
        <A href="/schedules" class="nav-link" activeClass="nav-active">
          ⏰ {t().nav.schedules}
        </A>
        <A href="/analytics" class="nav-link" activeClass="nav-active">
          📈 {t().nav.analytics}
        </A>
        <A href="/mcp" class="nav-link" activeClass="nav-active">
          🔌 {t().nav.mcp}
        </A>
        <A href="/settings" class="nav-link" activeClass="nav-active">
          ⚙ {t().nav.settings}
        </A>
      </div>

      <h3 class="section-title">Projects</h3>
      <Show when={!tasksQ.isPending} fallback={<p class="dim">loading…</p>}>
        <Show when={projects().length > 0} fallback={<p class="dim">no tasks yet</p>}>
          <For each={projects()}>
            {(proj) => (
              <div style="margin-bottom: 0.7rem">
                <div class="dim" style="font-size: 11px; margin-bottom: 0.2rem">
                  📁 {folderName(proj.workdir)}{' '}
                  <span class="fade">{proj.tasks.length}</span>
                </div>
                <For each={proj.tasks.slice(0, 4)}>
                  {(t) => (
                    <A
                      href={`/task/${t.id}`}
                      class="task-link"
                      activeClass="task-link-active"
                      title={t.goal}
                    >
                      {t.goal.slice(0, 38)}
                      {t.goal.length > 38 ? '…' : ''}
                    </A>
                  )}
                </For>
                <Show when={proj.tasks.length > 4}>
                  <span class="fade" style="font-size: 11px; padding-left: 0.5rem">
                    … +{proj.tasks.length - 4} more
                  </span>
                </Show>
              </div>
            )}
          </For>
        </Show>
      </Show>
    </div>
  );
};

export default SidebarLeft;
