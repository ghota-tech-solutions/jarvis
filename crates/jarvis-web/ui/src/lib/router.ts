import { lazy } from 'solid-js';
import type { RouteDefinition } from '@solidjs/router';

const Dashboard = lazy(() => import('~/routes/Dashboard'));
const Task = lazy(() => import('~/routes/Task'));
const Fleet = lazy(() => import('~/routes/Fleet'));

export const routes: RouteDefinition[] = [
  { path: '/', component: Dashboard },
  { path: '/fleet', component: Fleet },
  { path: '/task/:id', component: Task },
];
