// § F1.10 — task-verdict notification side-effect.
//
// Subscribes to TaskList query updates and detects verdict transitions
// (running/pending → completed/failed/cancelled). Each transition fires
// one OS notification via the adapter in `~/lib/notify.ts`, which prefers
// the Tauri plugin when available and falls back to the browser
// Notification API on the web surface.
//
// Permission is requested once on mount. Firing is gated on the user's
// `$notificationsEnabled` preference; when off, the subscription still
// runs (to keep `seen` in sync) but never calls `notify()`.

import { onMount } from 'solid-js';
import { useQueryClient } from '@tanstack/solid-query';
import type { Task } from '~/lib/api/gen/jarvis_pb';
import { $notificationsEnabled } from '~/lib/settings';
import { notify, requestNotificationPermission } from '~/lib/notify';

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
    // Ask once on mount via whichever backend is active. If denied, we
    // still wire the subscription so the "seen" tracker stays consistent
    // when permission is granted later in the session.
    await requestNotificationPermission();

    const seen = loadSeen();
    const unsub = qc.getQueryCache().subscribe((event) => {
      if (event.type !== 'updated') return;
      const data = event.query.state.data as { tasks?: Task[] } | undefined;
      if (!data?.tasks) return;
      for (const t of data.tasks) {
        const finished =
          t.status === 'completed' ||
          t.status === 'failed' ||
          t.status === 'cancelled';
        if (!finished) continue;
        if (seen[t.id] === t.status) continue;
        if (!seen[t.id]) {
          // Don't fire for tasks we've never seen — only for transitions.
          seen[t.id] = t.status;
          continue;
        }
        seen[t.id] = t.status;
        if (!$notificationsEnabled.get()) continue;
        const icon =
          t.status === 'completed' ? '✓' : t.status === 'failed' ? '✗' : '⊘';
        void notify({
          title: `${icon} ${t.goal.slice(0, 60)}`,
          body: `task ${t.status} · ${t.id.slice(0, 8)}`,
          tag: `task-${t.id}`,
        });
      }
      saveSeen(seen);
    });
    return () => unsub();
  });
}
