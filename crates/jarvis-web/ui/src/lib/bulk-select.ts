// § F1.7 — generic per-list bulk selection primitive.
//
// Each list (Dashboard tasks, Memory candidates/active, Schedules) needs
// its own multi-select state. This module returns a small reactive store
// + helpers; the rendering of checkboxes + action bar is on the consumer.
//
// Selection is in-memory only (no persistence) — clears on route change
// or refresh, which is the desired UX for "destructive ops on the current
// view". Generic on the ID type so it accepts both `string` (task ids,
// schedule ids) and `bigint` (memory ids exposed as protobuf uint64).

import { createSignal, type Accessor } from 'solid-js';

export type BulkSelect<TId> = {
  isSelected: (id: TId) => boolean;
  toggle: (id: TId) => void;
  selectAll: (ids: TId[]) => void;
  clear: () => void;
  count: Accessor<number>;
  ids: Accessor<TId[]>;
};

export function createBulkSelect<TId>(): BulkSelect<TId> {
  const [sel, setSel] = createSignal<Set<TId>>(new Set());
  return {
    isSelected: (id) => sel().has(id),
    toggle: (id) => {
      setSel((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      });
    },
    selectAll: (ids) => setSel(new Set(ids)),
    clear: () => setSel(new Set()),
    count: () => sel().size,
    ids: () => Array.from(sel()),
  };
}
