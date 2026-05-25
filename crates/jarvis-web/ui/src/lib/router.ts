import { lazy } from 'solid-js';
import type { RouteDefinition } from '@solidjs/router';

const Dashboard = lazy(() => import('~/routes/Dashboard'));
const Task = lazy(() => import('~/routes/Task'));
const Fleet = lazy(() => import('~/routes/Fleet'));
const Memory = lazy(() => import('~/routes/Memory'));
const Schedules = lazy(() => import('~/routes/Schedules'));
const Settings = lazy(() => import('~/routes/Settings'));
const Analytics = lazy(() => import('~/routes/Analytics'));

export const routes: RouteDefinition[] = [
  { path: '/', component: Dashboard },
  { path: '/fleet', component: Fleet },
  { path: '/memory', component: Memory },
  { path: '/schedules', component: Schedules },
  { path: '/analytics', component: Analytics },
  { path: '/settings', component: Settings },
  { path: '/task/:id', component: Task },
];
