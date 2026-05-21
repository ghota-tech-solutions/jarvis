import { createSignal } from 'solid-js';
import type { Task } from '~/lib/api/gen/jarvis_pb';

// localStorage keys
const PINNED_KEY = 'jarvis_pinned_tasks';
const ARCHIVED_KEY = 'jarvis_archived_tasks';
const UNREAD_KEY = 'jarvis_unread_tasks';
const NAMES_KEY = 'jarvis_task_names';
const SHOW_ARCHIVED_KEY = 'jarvis_show_archived';

// Helpers to load/save JSON from localStorage safely
function loadJSON<T>(key: string, defaultValue: T): T {
  try {
    const val = localStorage.getItem(key);
    return val ? JSON.parse(val) : defaultValue;
  } catch (e) {
    return defaultValue;
  }
}

function saveJSON<T>(key: string, val: T) {
  try {
    localStorage.setItem(key, JSON.stringify(val));
  } catch (e) {
    // Ignore storage errors
  }
}

// SolidJS signals for reactive UI updates
const [pinned, setPinned] = createSignal<string[]>(loadJSON(PINNED_KEY, []));
const [archived, setArchived] = createSignal<string[]>(loadJSON(ARCHIVED_KEY, []));
const [unread, setUnread] = createSignal<string[]>(loadJSON(UNREAD_KEY, []));
const [renamed, setRenamed] = createSignal<Record<string, string>>(loadJSON(NAMES_KEY, {}));
const [showArchived, setShowArchived] = createSignal<boolean>(loadJSON(SHOW_ARCHIVED_KEY, false));

export const taskStore = {
  // Pinned state
  pinned,
  isPinned(id: string): boolean {
    return pinned().includes(id);
  },
  togglePin(id: string) {
    const list = pinned();
    const updated = list.includes(id) ? list.filter(x => x !== id) : [...list, id];
    setPinned(updated);
    saveJSON(PINNED_KEY, updated);
  },

  // Archived state
  archived,
  isArchived(id: string): boolean {
    return archived().includes(id);
  },
  toggleArchive(id: string) {
    const list = archived();
    const updated = list.includes(id) ? list.filter(x => x !== id) : [...list, id];
    setArchived(updated);
    saveJSON(ARCHIVED_KEY, updated);
  },

  // Unread state
  unread,
  isUnread(id: string): boolean {
    return unread().includes(id);
  },
  toggleUnread(id: string) {
    const list = unread();
    const updated = list.includes(id) ? list.filter(x => x !== id) : [...list, id];
    setUnread(updated);
    saveJSON(UNREAD_KEY, updated);
  },
  markAsRead(id: string) {
    const list = unread();
    if (list.includes(id)) {
      const updated = list.filter(x => x !== id);
      setUnread(updated);
      saveJSON(UNREAD_KEY, updated);
    }
  },
  markAsUnread(id: string) {
    const list = unread();
    if (!list.includes(id)) {
      const updated = [...list, id];
      setUnread(updated);
      saveJSON(UNREAD_KEY, updated);
    }
  },

  // Custom Renames
  renamed,
  getCustomName(id: string): string | undefined {
    return renamed()[id];
  },
  renameTask(id: string, name: string) {
    const current = { ...renamed() };
    if (name.trim()) {
      current[id] = name.trim();
    } else {
      delete current[id];
    }
    setRenamed(current);
    saveJSON(NAMES_KEY, current);
  },

  // Show Archived settings
  showArchived,
  setShowArchived(val: boolean) {
    setShowArchived(val);
    saveJSON(SHOW_ARCHIVED_KEY, val);
  },

  // Helpers
  getDisplayName(t: Task): string {
    const custom = renamed()[t.id];
    if (custom) return custom;
    return t.goal;
  }
};
