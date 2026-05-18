import { For, Show, createMemo, type Component } from 'solid-js';
import type { TimelineEvent } from '~/lib/api/gen/jarvis_pb';

type Props = { events: TimelineEvent[] };

const kindIcon = (kind: string): string => {
  switch (kind) {
    case 'tool_call': return '→';
    case 'tool_result': return '✓';
    case 'error': return '✗';
    case 'verdict': return '■';
    case 'spawn': return '↪';
    case 'heartbeat': return '─';
    case 'attempt': return '·';
    case 'continuation': return '↻';
    default: return '';
  }
};

const kindClass = (kind: string): string => {
  switch (kind) {
    case 'decision': return 'evt-decision';
    case 'tool_call': return 'evt-tool-call';
    case 'tool_result': return 'evt-tool-result';
    case 'error': return 'evt-error';
    case 'verdict': return 'evt-verdict';
    case 'spawn': return 'evt-spawn';
    case 'heartbeat': return 'evt-heartbeat';
    case 'attempt': return 'evt-attempt';
    case 'continuation': return 'evt-continuation';
    default: return '';
  }
};

const parsePayload = (raw: string): Record<string, unknown> => {
  if (!raw) return {};
  try {
    return JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return { _raw: raw };
  }
};

const decisionText = (p: Record<string, unknown>): string => {
  const t = p.thought ?? p.text ?? p.content ?? p.message ?? '';
  return typeof t === 'string' ? t : JSON.stringify(t, null, 2);
};

const toolCallSummary = (p: Record<string, unknown>): { tool: string; args: string } => {
  const tool = typeof p.tool === 'string' ? p.tool : '?';
  const args = p.args && typeof p.args === 'object' ? p.args as Record<string, unknown> : {};
  for (const key of ['cmd', 'path', 'pattern', 'file', 'target']) {
    const v = args[key];
    if (typeof v === 'string') return { tool, args: v };
  }
  return { tool, args: JSON.stringify(args).slice(0, 120) };
};

const toolResultSummary = (p: Record<string, unknown>): {
  ok: boolean;
  exit?: number;
  output: string;
  bytes?: number;
} => {
  const exit = typeof p.exit_code === 'number' ? p.exit_code : undefined;
  const ok = exit === undefined ? p.error == null : exit === 0;
  const output =
    typeof p.output === 'string'
      ? p.output
      : typeof p.stdout === 'string'
        ? p.stdout
        : typeof p.text === 'string'
          ? p.text
          : JSON.stringify(p).slice(0, 200);
  const bytes = typeof p.bytes === 'number' ? p.bytes : undefined;
  return { ok, exit, output, bytes };
};

const EventBlock: Component<{ evt: TimelineEvent }> = (p) => {
  const payload = createMemo(() => parsePayload(p.evt.payloadJson));

  return (
    <div class={`evt-block ${kindClass(p.evt.kind)}`}>
      <Show when={p.evt.kind === 'decision'}>
        <div class="evt-prose">{decisionText(payload())}</div>
      </Show>

      <Show when={p.evt.kind === 'tool_call'}>
        {(() => {
          const s = toolCallSummary(payload());
          return (
            <div>
              <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
              <span class="evt-tool">{s.tool}</span>
              <span class="evt-args">{s.args}</span>
            </div>
          );
        })()}
      </Show>

      <Show when={p.evt.kind === 'tool_result'}>
        {(() => {
          const s = toolResultSummary(payload());
          return (
            <div class={s.ok ? 'evt-ok' : 'evt-fail'}>
              <span class="evt-icon">{s.ok ? '✓' : '✗'}</span>
              <Show when={s.exit !== undefined}>
                <span class="dim">exit {s.exit}</span>
              </Show>
              <Show when={s.bytes !== undefined}>
                <span class="dim">{s.bytes} B</span>
              </Show>
              <details class="evt-collapse">
                <summary class="dim">output</summary>
                <pre class="evt-output">{s.output}</pre>
              </details>
            </div>
          );
        })()}
      </Show>

      <Show when={p.evt.kind === 'error'}>
        <div class="evt-fail">
          <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
          <pre class="evt-output">{JSON.stringify(payload(), null, 2)}</pre>
        </div>
      </Show>

      <Show when={p.evt.kind === 'verdict'}>
        {(() => {
          const v = payload().verdict;
          const ok = v === 'pass' || v === 'done';
          return (
            <div class={ok ? 'evt-ok' : 'evt-fail'}>
              <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
              <span class={`pill ${ok ? 'good' : 'error'}`}>{String(v)}</span>
              <Show when={typeof payload().message === 'string'}>
                <span class="dim"> · {String(payload().message)}</span>
              </Show>
            </div>
          );
        })()}
      </Show>

      <Show when={p.evt.kind === 'heartbeat' || p.evt.kind === 'attempt'}>
        <div class="evt-heartbeat">
          <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
          <span class="fade">
            step {String(payload().step ?? '?')}
          </span>
        </div>
      </Show>

      <Show when={p.evt.kind === 'spawn'}>
        <div class="evt-spawn">
          <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
          <span class="dim">spawned subtask</span>
        </div>
      </Show>

      <Show when={p.evt.kind === 'continuation'}>
        <div class="evt-continuation">
          <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
          <span class="warn">continuation: {String(payload().reason ?? '?')}</span>
        </div>
      </Show>
    </div>
  );
};

const Transcript: Component<Props> = (p) => {
  const filtered = createMemo(() =>
    // Drop llm_chunk events — those would flood the transcript. They are
    // collapsed into surrounding decisions by the agent loop already.
    p.events.filter((e) => e.kind !== 'llm_chunk')
  );

  return (
    <div class="transcript">
      <For each={filtered()}>{(evt) => <EventBlock evt={evt} />}</For>
    </div>
  );
};

export default Transcript;
