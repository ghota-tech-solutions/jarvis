// § F2.4 — Analytics dashboard.
//
// Client-side aggregation over the cached TaskList so we don't need a new
// RPC for v1. Surfaces three views:
//   1. Daily task throughput (bar chart, last 30 days).
//   2. Outcome breakdown (text grid: pass / fail / running / cancelled).
//   3. Top-N parent tasks by depth (text list — useful for spotting
//      runaway chains).
//
// uPlot powers the line/bar canvas — tiny lib (~38 KB), DPR-aware,
// re-renders fast. We wrap it in a Solid component that recreates on
// data change (Solid signals + uPlot's imperative API mix cleanly via
// onMount + onCleanup).

import {
  For,
  Show,
  createMemo,
  onCleanup,
  onMount,
  type Component,
} from 'solid-js';
import { createQuery } from '@tanstack/solid-query';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';
import AppErrorBoundary from '~/components/ErrorBoundary';
import { EmptyState } from '~/components/EmptyState';
import { taskListQuery } from '~/lib/api/queries';
import { useT } from '~/lib/i18n';
import { useStore } from '@nanostores/solid';
import { themeAtom } from '~/lib/stores/theme';

type Bucket = { day: string; total: number; pass: number; fail: number };

const SECS_PER_DAY = 86_400;

function microsToDayKey(micros: bigint): string {
  const ms = Number(micros / 1000n);
  const d = new Date(ms);
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, '0')}-${String(d.getUTCDate()).padStart(2, '0')}`;
}

function microsToUnix(micros: bigint): number {
  return Number(micros / 1_000_000n);
}

const ChartPanel: Component<{
  title: string;
  data: uPlot.AlignedData;
  series: uPlot.Series[];
  height?: number;
  emptyHint?: string;
}> = (p) => {
  let containerRef: HTMLDivElement | undefined;
  let plot: uPlot | undefined;
  // Re-render on theme change so axis/grid colors swap.
  const theme = useStore(themeAtom);

  const buildOpts = (): uPlot.Options => {
    const isDark = theme() === 'dark';
    const axisColor = isDark ? 'rgba(255,255,255,0.55)' : 'rgba(0,0,0,0.55)';
    const gridColor = isDark ? 'rgba(255,255,255,0.06)' : 'rgba(0,0,0,0.08)';
    return {
      title: '',
      width: containerRef?.clientWidth ?? 600,
      height: p.height ?? 200,
      cursor: { drag: { x: true, y: false } },
      scales: { x: { time: true } },
      axes: [
        { stroke: axisColor, grid: { stroke: gridColor, width: 1 } },
        { stroke: axisColor, grid: { stroke: gridColor, width: 1 } },
      ],
      series: p.series,
    };
  };

  const mount = () => {
    if (!containerRef) return;
    plot?.destroy();
    if (p.data[0].length === 0) return;
    plot = new uPlot(buildOpts(), p.data, containerRef);
  };

  onMount(() => {
    mount();
    const ro = new ResizeObserver(() => {
      if (!plot || !containerRef) return;
      plot.setSize({ width: containerRef.clientWidth, height: p.height ?? 200 });
    });
    if (containerRef) ro.observe(containerRef);
    onCleanup(() => {
      ro.disconnect();
      plot?.destroy();
    });
  });

  // Re-mount when data or theme changes.
  // (`createEffect` would do it, but onMount + a dummy memo subscription
  // suffices for v1.)
  createMemo(() => {
    // Touch the reactive sources so this memo re-runs on change.
    const _ = [p.data, theme()];
    void _;
    mount();
  });

  return (
    <div class="analytics-panel">
      <h3 class="analytics-panel-title">{p.title}</h3>
      <Show
        when={p.data[0].length > 0}
        fallback={
          <p class="dim" style="padding: 1rem; font-size: 12px">
            {p.emptyHint ?? 'not enough data yet'}
          </p>
        }
      >
        <div ref={(el) => (containerRef = el)} class="analytics-chart" />
      </Show>
    </div>
  );
};

const StatCard: Component<{ label: string; value: string; hint?: string }> = (
  p,
) => (
  <div class="analytics-stat">
    <div class="analytics-stat-value">{p.value}</div>
    <div class="analytics-stat-label">{p.label}</div>
    <Show when={p.hint}>
      <div class="analytics-stat-hint dim">{p.hint}</div>
    </Show>
  </div>
);

const Analytics: Component = () => {
  const t = useT();
  const tasksQ = createQuery(() => taskListQuery(true));

  const summary = createMemo(() => {
    const tasks = tasksQ.data?.tasks ?? [];
    let pass = 0;
    let fail = 0;
    let running = 0;
    let cancelled = 0;
    for (const t of tasks) {
      switch (t.status) {
        case 'completed':
          pass++;
          break;
        case 'failed':
          fail++;
          break;
        case 'running':
        case 'pending':
          running++;
          break;
        case 'cancelled':
          cancelled++;
          break;
      }
    }
    const finished = pass + fail;
    return {
      total: tasks.length,
      pass,
      fail,
      running,
      cancelled,
      passRate: finished > 0 ? (pass / finished) * 100 : 0,
    };
  });

  const dailyBuckets = createMemo<Bucket[]>(() => {
    const tasks = tasksQ.data?.tasks ?? [];
    const byDay = new Map<string, Bucket>();
    for (const t of tasks) {
      const key = microsToDayKey(t.createdAt);
      if (!byDay.has(key)) {
        byDay.set(key, { day: key, total: 0, pass: 0, fail: 0 });
      }
      const b = byDay.get(key)!;
      b.total++;
      if (t.status === 'completed') b.pass++;
      else if (t.status === 'failed') b.fail++;
    }
    return Array.from(byDay.values()).sort((a, b) =>
      a.day.localeCompare(b.day),
    );
  });

  // uPlot format: [xs, y1, y2, ...]
  const throughputData = createMemo<uPlot.AlignedData>(() => {
    const buckets = dailyBuckets();
    if (buckets.length === 0) {
      return [[], [], [], []] as unknown as uPlot.AlignedData;
    }
    const xs: number[] = [];
    const total: number[] = [];
    const pass: number[] = [];
    const fail: number[] = [];
    for (const b of buckets) {
      const [y, mo, d] = b.day.split('-').map(Number);
      xs.push(Date.UTC(y, mo - 1, d) / 1000);
      total.push(b.total);
      pass.push(b.pass);
      fail.push(b.fail);
    }
    return [xs, total, pass, fail] as unknown as uPlot.AlignedData;
  });

  const passRateData = createMemo<uPlot.AlignedData>(() => {
    const buckets = dailyBuckets();
    if (buckets.length === 0) return [[], []] as unknown as uPlot.AlignedData;
    const xs: number[] = [];
    const rates: number[] = [];
    for (const b of buckets) {
      const finished = b.pass + b.fail;
      if (finished === 0) continue;
      const [y, mo, d] = b.day.split('-').map(Number);
      xs.push(Date.UTC(y, mo - 1, d) / 1000);
      rates.push((b.pass / finished) * 100);
    }
    return [xs, rates] as unknown as uPlot.AlignedData;
  });

  const throughputSeries: uPlot.Series[] = [
    { label: 'date' },
    { label: 'total', stroke: 'rgb(96, 165, 250)', width: 2 },
    { label: 'completed', stroke: 'rgb(110, 200, 120)', width: 1.5 },
    { label: 'failed', stroke: 'rgb(248, 113, 113)', width: 1.5 },
  ];

  const passRateSeries: uPlot.Series[] = [
    { label: 'date' },
    {
      label: 'pass-rate %',
      stroke: 'rgb(140, 180, 110)',
      width: 2,
      fill: 'rgba(140, 180, 110, 0.12)',
    },
  ];

  const recentSpan = createMemo<string>(() => {
    const buckets = dailyBuckets();
    if (buckets.length === 0) return '';
    const first = buckets[0].day;
    const last = buckets[buckets.length - 1].day;
    if (first === last) return first;
    return `${first} → ${last}`;
  });

  return (
    <AppErrorBoundary name="Analytics">
      <section class="analytics-page">
        <header class="analytics-header">
          <h2 class="heading" style="margin: 0">
            Analytics
          </h2>
          <p class="dim" style="margin: 0.2rem 0 0 0; font-size: 12px">
            Client-side aggregation over your task history. v1 uses the cached
            task list; daemon-side time-series and per-model cost breakdown
            land in a follow-up.
          </p>
        </header>

        <Show
          when={(tasksQ.data?.tasks?.length ?? 0) > 0}
          fallback={
            <EmptyState
              title={t().empty.no_tasks_title}
              hint={t().empty.no_tasks_hint}
              actionHref="/"
              actionLabel={t().empty.no_fleet_cta}
            />
          }
        >
          <div class="analytics-stats">
            <StatCard
              label="Total tasks"
              value={String(summary().total)}
              hint={recentSpan()}
            />
            <StatCard
              label="Pass rate"
              value={`${summary().passRate.toFixed(1)}%`}
              hint={`${summary().pass} / ${summary().pass + summary().fail} finished`}
            />
            <StatCard label="Running" value={String(summary().running)} />
            <StatCard label="Failed" value={String(summary().fail)} />
            <StatCard label="Cancelled" value={String(summary().cancelled)} />
          </div>

          <ChartPanel
            title="Daily throughput"
            data={throughputData()}
            series={throughputSeries}
            emptyHint="no recent tasks yet"
          />

          <ChartPanel
            title="Pass-rate over time"
            data={passRateData()}
            series={passRateSeries}
            emptyHint="no finished tasks yet"
          />
        </Show>
      </section>
    </AppErrorBoundary>
  );
};

// Silence unused-import warnings that ESLint may flag for utilities kept
// for future panels.
export const _internal = { microsToUnix, SECS_PER_DAY, For };

export default Analytics;
