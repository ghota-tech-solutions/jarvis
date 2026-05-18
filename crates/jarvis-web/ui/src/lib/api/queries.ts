// Shared TanStack Query factories for the Connect API.
//
// Each factory returns a query options object that can be passed to
// `createQuery` from `@tanstack/solid-query`. Centralizing keys here keeps
// invalidation cross-component without surprising stale-key bugs.

import { jarvis } from './client';

export const qkPing = () => ['ping'] as const;
export const qkTaskList = (includeFinished: boolean) =>
  ['tasks', { includeFinished }] as const;
export const qkTask = (id: string) => ['task', id] as const;
export const qkStatus = () => ['status'] as const;
export const qkTimeline = (id: string) => ['timeline', id] as const;

export const taskListQuery = (includeFinished = true) => ({
  queryKey: qkTaskList(includeFinished),
  queryFn: async () =>
    await jarvis.listTasks({ includeFinished, limit: 200 }),
  refetchInterval: 2000,
});

export const statusQuery = () => ({
  queryKey: qkStatus(),
  queryFn: async () => await jarvis.getStatus({}),
  refetchInterval: 5000,
});

export const taskQuery = (id: string) => ({
  queryKey: qkTask(id),
  queryFn: async () => await jarvis.getTask({ id }),
  refetchInterval: 2000,
});

export const timelineQuery = (id: string) => ({
  queryKey: qkTimeline(id),
  queryFn: async () => await jarvis.getTimeline({ id }),
  refetchInterval: 3000,
});
