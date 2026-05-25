// § F2.2 — Recipes Library.
//
// Recipes (T2.4 backend) are YAML files in `<data_dir>/recipes/` that the
// daemon parses at startup and upserts as schedules with id `recipe:*`.
// This page filters `ListSchedules` to those entries and renders them as
// cards. Edits are NOT performed here: the recipe YAML is the source of
// truth, so we link to the path and tell the user to edit on disk +
// restart the daemon. Run-now and delete operations reuse the existing
// schedule RPCs.

import { For, Show, createSignal, type Component } from 'solid-js';
import { createQuery, useQueryClient } from '@tanstack/solid-query';
import { jarvis } from '~/lib/api/client';
import type { Schedule } from '~/lib/api/gen/jarvis_pb';
import AppErrorBoundary from '~/components/ErrorBoundary';
import { EmptyState } from '~/components/EmptyState';
import { useT } from '~/lib/i18n';

const RECIPE_ID_PREFIX = 'recipe:';

function fmtMicros(micros: bigint): string {
  if (!micros) return '—';
  const n = Number(micros / 1000n);
  const d = new Date(n);
  return d.toLocaleString();
}

const RecipeCard: Component<{ s: Schedule; onChange: () => void }> = (p) => {
  const spec = () => p.s.spec!;
  const recipeName = () => spec().id.slice(RECIPE_ID_PREFIX.length);
  const [busy, setBusy] = createSignal(false);

  const runNow = async () => {
    if (busy()) return;
    setBusy(true);
    try {
      await jarvis.runScheduleNow({ id: spec().id });
      p.onChange();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class={`recipe-card ${spec().paused ? 'is-paused' : ''}`}>
      <header class="recipe-card-header">
        <span class="recipe-card-name">{recipeName()}</span>
        <Show when={spec().paused}>
          <span class="pill warn">paused</span>
        </Show>
        <code class="dim recipe-card-cron">{spec().cron}</code>
      </header>
      <Show when={spec().label && spec().label !== recipeName()}>
        <p class="recipe-card-desc">{spec().label}</p>
      </Show>
      <p class="recipe-card-goal">{spec().goal}</p>
      <div class="recipe-card-meta">
        <span class="dim">workdir:</span>
        <code title={spec().workdir}>{spec().workdir}</code>
      </div>
      <div class="recipe-card-meta">
        <span class="dim">sandbox:</span>
        <code>{spec().sandbox || 'native'}</code>
        <span class="fade">·</span>
        <span class="dim">routing:</span>
        <code>{spec().routingPolicy || 'auto'}</code>
        <span class="fade">·</span>
        <span class="dim">max_steps:</span>
        <code>{spec().maxSteps || '—'}</code>
      </div>
      <div class="recipe-card-meta">
        <span class="dim">next:</span>
        <span class={spec().paused ? 'fade' : 'warn'}>
          {fmtMicros(p.s.nextRunMicros)}
        </span>
        <span class="fade">·</span>
        <span class="dim">last:</span>
        <span>{fmtMicros(p.s.lastRunMicros)}</span>
      </div>
      <div class="recipe-card-actions">
        <button
          type="button"
          class="btn"
          disabled={busy() || spec().paused}
          onClick={() => void runNow()}
        >
          {busy() ? 'Running…' : 'Run now'}
        </button>
      </div>
    </div>
  );
};

const Recipes: Component = () => {
  const t = useT();
  const qc = useQueryClient();
  const q = createQuery(() => ({
    queryKey: ['schedules'],
    queryFn: async () => await jarvis.listSchedules({}),
    refetchInterval: 5_000,
  }));

  const recipes = () =>
    (q.data?.schedules ?? []).filter((s) =>
      s.spec?.id.startsWith(RECIPE_ID_PREFIX),
    );

  return (
    <AppErrorBoundary name="Recipes">
      <section class="recipes-page">
        <header class="recipes-header">
          <h2 class="heading" style="margin: 0">
            Recipes
          </h2>
          <p class="dim" style="margin: 0.2rem 0 0 0; font-size: 12px">
            Declarative YAML workflows under{' '}
            <code>&lt;data_dir&gt;/recipes/</code>. The YAML is the source of
            truth — edit a file and restart the daemon to update the
            schedule. Run-now triggers an immediate task without waiting for
            the cron.
          </p>
        </header>

        <Show
          when={recipes().length > 0}
          fallback={
            <EmptyState
              title="No recipes loaded"
              hint="Drop a YAML file into <data_dir>/recipes/ (the filename stem must match the recipe name) and restart the daemon. See recipes-example.yaml at the repo root for the format."
            />
          }
        >
          <div class="recipes-grid">
            <For each={recipes()}>
              {(s) => (
                <RecipeCard
                  s={s}
                  onChange={() =>
                    qc.invalidateQueries({ queryKey: ['schedules'] })
                  }
                />
              )}
            </For>
          </div>
        </Show>
        {/* Suppress unused-import warnings from i18n while the page strings
            stay short and in-file. The dict already has labels for nav,
            but the page copy itself is brief enough to live inline. */}
        <span hidden>{t().nav.dashboard}</span>
      </section>
    </AppErrorBoundary>
  );
};

export default Recipes;
