// CodeMirror 6 side-by-side merge view, wrapped as a Solid component.
//
// We mount one MergeView per FileDiff. The editor is destroyed and
// rebuilt when `before` or `after` change — these strings are small
// enough (per-file content) that the rebuild is cheap.

import { MergeView } from '@codemirror/merge';
import { EditorState } from '@codemirror/state';
import { onCleanup, onMount, createEffect, type Component } from 'solid-js';

type Props = {
  before: string;
  after: string;
  readonly?: boolean;
};

const MergeViewer: Component<Props> = (p) => {
  let container!: HTMLDivElement;
  let view: MergeView | null = null;

  const build = () => {
    if (view) {
      view.destroy();
      view = null;
    }
    view = new MergeView({
      parent: container,
      a: {
        doc: p.before,
        extensions: [EditorState.readOnly.of(true)],
      },
      b: {
        doc: p.after,
        extensions: [EditorState.readOnly.of(p.readonly !== false)],
      },
      gutter: true,
      revertControls: undefined,
      highlightChanges: true,
      collapseUnchanged: { margin: 3, minSize: 6 },
    });
  };

  onMount(build);
  createEffect(() => {
    // Re-evaluate on prop changes
    void p.before;
    void p.after;
    build();
  });
  onCleanup(() => {
    if (view) view.destroy();
  });

  return <div ref={container} class="merge-host" />;
};

export default MergeViewer;
