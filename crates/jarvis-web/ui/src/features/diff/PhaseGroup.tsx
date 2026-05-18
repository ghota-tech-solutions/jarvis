import { For, Show, createSignal, type Component } from 'solid-js';
import { jarvis } from '~/lib/api/client';
import { useQueryClient } from '@tanstack/solid-query';
import type { DiffGroup } from '~/lib/api/gen/jarvis_pb';
import MergeViewer from './MergeView';

type Props = {
  taskId: string;
  group: DiffGroup;
  onChange?: () => void;
};

const PhaseGroup: Component<Props> = (p) => {
  const qc = useQueryClient();
  const [expanded, setExpanded] = createSignal<Record<string, boolean>>({});
  const [subject, setSubject] = createSignal('');
  const [busy, setBusy] = createSignal<'commit' | 'reject' | null>(null);
  const [info, setInfo] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  const totalAdded = () =>
    p.group.files.reduce((a, f) => a + f.linesAdded, 0);
  const totalRemoved = () =>
    p.group.files.reduce((a, f) => a + f.linesRemoved, 0);

  const onCommit = async () => {
    setBusy('commit');
    setError(null);
    setInfo(null);
    try {
      const res = await jarvis.commitPhase({
        taskId: p.taskId,
        decisionEvtId: p.group.decisionEvtId,
        subject: subject().trim(),
      });
      setInfo(`✓ commit ${res.commitSha.slice(0, 8)} on ${res.branch}`);
      await qc.invalidateQueries({ queryKey: ['diff', p.taskId] });
      p.onChange?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const onReject = async () => {
    if (
      !confirm(
        `Revert ${p.group.files.length} file(s) in phase ${p.group.step}? ` +
          `Changes will be lost.`,
      )
    ) {
      return;
    }
    setBusy('reject');
    setError(null);
    setInfo(null);
    try {
      await jarvis.rejectPhase({
        taskId: p.taskId,
        decisionEvtId: p.group.decisionEvtId,
        subject: '',
      });
      setInfo('phase reverted');
      await qc.invalidateQueries({ queryKey: ['diff', p.taskId] });
      p.onChange?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <article class="phase-group">
      <header class="phase-header">
        <span class="pill accent">phase {p.group.step || '–'}</span>
        <span class="dim" style="font-size: 11px">
          evt #{Number(p.group.decisionEvtId)} · {p.group.files.length} file(s)
        </span>
        <span class="good" style="font-size: 11px; margin-left: auto">
          +{totalAdded()}
        </span>
        <span class="error" style="font-size: 11px">
          −{totalRemoved()}
        </span>
      </header>
      <Show when={p.group.decisionText}>
        <p class="phase-decision">{p.group.decisionText.slice(0, 360)}</p>
      </Show>
      <ul class="phase-files">
        <For each={p.group.files}>
          {(file) => (
            <li>
              <button
                type="button"
                class="phase-file-toggle"
                onClick={() =>
                  setExpanded((e) => ({ ...e, [file.path]: !e[file.path] }))
                }
              >
                <span class={`pill ${changeKindClass(file.changeKind)}`}>
                  {file.changeKind[0].toUpperCase()}
                </span>
                <code style="margin: 0 0.4rem">{file.path}</code>
                <span class="good" style="font-size: 11px">
                  +{file.linesAdded}
                </span>
                <span class="error" style="font-size: 11px">
                  −{file.linesRemoved}
                </span>
                <span class="dim" style="margin-left: auto; font-size: 11px">
                  {expanded()[file.path] ? '▾ collapse' : '▸ expand'}
                </span>
              </button>
              <Show when={expanded()[file.path]}>
                <MergeViewer before={file.before} after={file.after} readonly />
              </Show>
            </li>
          )}
        </For>
      </ul>
      <div class="phase-actions">
        <input
          class="input"
          placeholder="commit subject — empty = synth from decision"
          value={subject()}
          onInput={(e) => setSubject(e.currentTarget.value)}
        />
        <button
          type="button"
          class="btn ghost"
          onClick={onReject}
          disabled={busy() !== null}
        >
          {busy() === 'reject' ? 'reverting…' : 'reject phase'}
        </button>
        <button
          type="button"
          class="btn"
          onClick={onCommit}
          disabled={busy() !== null}
        >
          {busy() === 'commit' ? 'committing…' : 'commit phase'}
        </button>
      </div>
      <Show when={info()}>
        <p class="good" style="font-size: 12px">
          {info()}
        </p>
      </Show>
      <Show when={error()}>
        <p class="error" style="font-size: 12px">
          {error()}
        </p>
      </Show>
    </article>
  );
};

function changeKindClass(k: string): 'good' | 'warn' | 'error' | '' {
  switch (k) {
    case 'added': return 'good';
    case 'modified': return 'warn';
    case 'deleted': return 'error';
    default: return '';
  }
}

export default PhaseGroup;
