import { lazy } from 'solid-js';
import type { RouteDefinition } from '@solidjs/router';

const Dashboard = lazy(() => import('~/routes/Dashboard'));
const Task = lazy(() => import('~/routes/Task'));

export const routes: RouteDefinition[] = [
  { path: '/', component: Dashboard },
  { path: '/task/:id', component: Task },
];
