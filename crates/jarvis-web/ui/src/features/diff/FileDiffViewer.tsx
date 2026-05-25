// M8.1: lazy, paginated viewer for one file's unified diff.
//
// The first chunk (200 lines) is fetched on mount; if the server reports
// `has_more` we expose a "load more" button that appends the next slice
// to the accumulated diff text. The previous implementation shipped full
// file contents inside the DiffGroup response and rendered them through
// `@codemirror/merge`; large refactors used to make that response huge.
//
// § F1.8: rendered via Shiki (lazy-loaded) with the `diff` grammar — gives
// proper colors for +/-/@@/--- lines via the active theme. Falls back to a
// manually-classed <pre> rendering while Shiki is loading or if it errors.

import {
  For,
  Show,
  createEffect,
  createSignal,
  onMount,
  type Component,
} from 'solid-js';
import { jarvis } from '~/lib/api/client';
import type { FileDiff } from '~/lib/api/gen/jarvis_pb';
import { highlightDiff } from '~/lib/highlight';

type Props = {
  taskId: string;
  groupEvtId: bigint;
  file: FileDiff;
};

const CHUNK = 200;

const FileDiffViewer: Component<Props> = (p) => {
  const [text, setText] = createSignal('');
  const [loaded, setLoaded] = createSignal(0);
  const [total, setTotal] = createSignal(p.file.totalLines);
  const [hasMore, setHasMore] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [html, setHtml] = createSignal<string | null>(null);

  // Re-highlight whenever the accumulated diff text grows. The async call
  // returns the latest text rendered; a stale resolve can land out of order
  // (e.g. user spams "load more"). We guard with a token to drop staler
  // results.
  let highlightToken = 0;
  createEffect(() => {
    const t = text();
    if (!t) {
      setHtml(null);
      return;
    }
    const token = ++highlightToken;
    void highlightDiff(t).then((out) => {
      if (token === highlightToken) setHtml(out);
    });
  });

  const loadChunk = async () => {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      const res = await jarvis.getFileDiff({
        taskId: p.taskId,
        // The proto field is uint64 → bigint in TS. `decisionEvtId` is
        // already a bigint coming from the DiffGroup; reuse as-is.
        groupEvtId: BigInt(p.groupEvtId),
        path: p.file.path,
        offsetLines: loaded(),
        maxLines: CHUNK,
      });
      // Append; `res.content` is already \n-joined and not terminated.
      setText((prev) => (prev ? prev + '\n' : '') + res.content);
      setLoaded((n) => n + res.returnedLines);
      setTotal(res.totalLines);
      setHasMore(res.hasMore);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  onMount(() => {
    if (p.file.totalLines === 0) {
      // Server reports an empty diff (e.g. binary file or identical
      // contents); nothing to fetch.
      setLoaded(0);
      setHasMore(false);
      return;
    }
    void loadChunk();
  });

  return (
    <div class="file-diff-viewer">
      <Show when={error()}>
        <p class="error" style="font-size: 12px">{error()}</p>
      </Show>
      <Show when={total() === 0 && !error()}>
        <p class="dim" style="font-size: 12px; padding: 0.4rem">
          (empty diff — likely a binary file or whitespace-only change)
        </p>
      </Show>
      <Show
        when={text()}
      >
        <Show
          when={html()}
          fallback={
            // Plain-text fallback while Shiki is loading the grammar or if
            // it errored. Uses the manual per-line class coloring as before
            // so the user always sees +/-/@@ markers.
            <pre class="unified-diff">
              <For each={text().split('\n')}>
                {(line) => <DiffLine line={line} />}
              </For>
            </pre>
          }
        >
          {/* eslint-disable-next-line solid/no-innerhtml */}
          <div class="diff-shiki" innerHTML={html() ?? ''} />
        </Show>
      </Show>
      <Show when={hasMore()}>
        <button
          type="button"
          class="btn ghost"
          onClick={() => loadChunk()}
          disabled={busy()}
          style="margin-top: 0.4rem"
        >
          {busy()
            ? 'loading…'
            : `load more (${loaded()} / ${total()} lines)`}
        </button>
      </Show>
    </div>
  );
};

const DiffLine: Component<{ line: string }> = (p) => {
  const cls = (): string => {
    const c = p.line.charAt(0);
    if (p.line.startsWith('+++') || p.line.startsWith('---')) return 'diff-file';
    if (p.line.startsWith('@@')) return 'diff-hunk';
    if (c === '+') return 'diff-add';
    if (c === '-') return 'diff-del';
    return 'diff-ctx';
  };
  return <span class={cls()}>{p.line}{'\n'}</span>;
};

export default FileDiffViewer;
