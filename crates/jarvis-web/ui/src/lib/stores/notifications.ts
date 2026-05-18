// Browser-notification side-effect.
//
// Subscribes to TaskList + GetStatus to detect verdict transitions
// (running/pending → completed/failed/cancelled) and fires a Notification.
// The user must grant permission first; if denied, this is a silent no-op.

import { onMount } from 'solid-js';
import { useQueryClient } from '@tanstack/solid-query';
import type { Task } from '~/lib/api/gen/jarvis_pb';

const STORAGE_KEY = 'jarvis-notif-seen';

function loadSeen(): Record<string, string> {
  try {
    return JSON.parse(sessionStorage.getItem(STORAGE_KEY) ?? '{}');
  } catch {
    return {};
  }
}

function saveSeen(seen: Record<string, string>) {
  sessionStorage.setItem(STORAGE_KEY, JSON.stringify(seen));
}

export function useTaskNotifications() {
  const qc = useQueryClient();

  onMount(async () => {
    if (typeof Notification === 'undefined') return;
    if (Notification.permission === 'default') {
      // Ask once on mount. If denied, we never ask again this session.
      try {
        await Notification.requestPermission();
      } catch {
        /* ignored */
      }
    }
    if (Notification.permission !== 'granted') return;

    const seen = loadSeen();
    const unsub = qc.getQueryCache().subscribe((event) => {
      if (event.type !== 'updated') return;
      const data = event.query.state.data as { tasks?: Task[] } | undefined;
      if (!data?.tasks) return;
      for (const t of data.tasks) {
        const finished = t.status === 'completed' || t.status === 'failed' || t.status === 'cancelled';
        if (!finished) continue;
        if (seen[t.id] === t.status) continue;
        if (!seen[t.id]) {
          // Don't fire for tasks we've never seen — only for transitions.
          seen[t.id] = t.status;
          continue;
        }
        seen[t.id] = t.status;
        const icon = t.status === 'completed' ? '✓' : t.status === 'failed' ? '✗' : '⊘';
        new Notification(`${icon} ${t.goal.slice(0, 60)}`, {
          body: `task ${t.status} · ${t.id.slice(0, 8)}`,
          tag: `task-${t.id}`,
        });
      }
      saveSeen(seen);
    });
    return () => unsub();
  });
}
