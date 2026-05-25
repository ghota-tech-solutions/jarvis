// § F1.7 — sticky action bar shown when at least one item is selected.
//
// Renders the selection count, a "Clear" button, and any number of
// caller-defined action buttons. Pure presentational — the consumer owns
// the selection store and the action callbacks.

import { For, Show, type Component } from 'solid-js';

export type BulkAction = {
  /** Visible label. */
  label: string;
  /** Click handler. Async callbacks are awaited; the bar disables actions
   *  while any is running. */
  onClick: () => void | Promise<void>;
  /** Visual variant for the button. */
  variant?: 'primary' | 'danger' | 'ghost';
  /** Optional confirm text — when set, a `window.confirm(text)` gate runs
   *  before `onClick`. */
  confirm?: string;
};

type Props = {
  /** Number of currently-selected items. The bar hides when 0. */
  count: number;
  /** Reset selection. */
  onClear: () => void;
  /** Action buttons rendered right of the count. */
  actions: BulkAction[];
  /** Optional disable-all flag (e.g. while a parent mutation is in flight). */
  busy?: boolean;
};

const BulkActionsBar: Component<Props> = (p) => {
  const onActionClick = async (a: BulkAction) => {
    if (a.confirm && !window.confirm(a.confirm)) return;
    await a.onClick();
  };
  return (
    <Show when={p.count > 0}>
      <div class="bulk-actions-bar" role="region" aria-label="Bulk actions">
        <span class="bulk-count">
          <strong>{p.count}</strong> selected
        </span>
        <button
          type="button"
          class="btn ghost bulk-clear"
          onClick={() => p.onClear()}
          disabled={p.busy}
        >
          Clear
        </button>
        <span class="bulk-spacer" />
        <For each={p.actions}>
          {(a) => (
            <button
              type="button"
              class={`btn ${a.variant ?? 'primary'}`}
              disabled={p.busy}
              onClick={() => void onActionClick(a)}
            >
              {a.label}
            </button>
          )}
        </For>
      </div>
    </Show>
  );
};

export default BulkActionsBar;
