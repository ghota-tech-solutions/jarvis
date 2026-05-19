import { For, Show, createMemo, type Component } from 'solid-js';
import type { TimelineEvent } from '~/lib/api/gen/jarvis_pb';
import Markdown from './Markdown';

type Props = {
  events: TimelineEvent[];
  selectedEvtId?: number;
  // § C UX — goal text per task id, so a follow-up chain renders a
  // "user turn" header between each task's events.
  taskGoals?: Record<string, string>;
};

// A render item is either an event block or a conversation-turn header
// inserted when the task_id changes between consecutive events.
type Row =
  | { kind: 'event'; evt: TimelineEvent }
  | { kind: 'turn'; taskId: string; goal: string; index: number };

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

const EventBlock: Component<{ evt: TimelineEvent; selected?: boolean }> = (p) => {
  const payload = createMemo(() => parsePayload(p.evt.payloadJson));

  return (
    <div
      class={`evt-block ${kindClass(p.evt.kind)} ${p.selected ? 'evt-selected' : ''}`}
      data-evt-id={Number(p.evt.id)}
    >
      <Show when={p.evt.kind === 'decision'}>
        {(() => {
          const pl = payload();
          const thought = typeof pl.thought === 'string' ? pl.thought.trim() : '';
          const message = typeof pl.message === 'string' ? pl.message.trim() : '';
          const action = typeof pl.action === 'string' ? pl.action : '';
          const answerLabel =
            action === 'done' ? 'answer' : action === 'fail' ? 'gave up' : 'note';
          const hasMessage = message !== '' && message !== thought;
          return (
            <div class="evt-decision-body">
              {/* `thought` = the model's internal reasoning (often English
                  even when the answer is French). Muted + secondary so it
                  doesn't compete visually with the real answer. */}
              <Show when={thought}>
                <div class="evt-thought">
                  <span class="evt-label">reasoning</span>
                  <div class="evt-thought-body">
                    <Markdown text={thought} />
                  </div>
                </div>
              </Show>
              {/* `message` = the user-facing answer (done / fail). Given a
                  prominent container so it reads as THE answer. */}
              <Show when={hasMessage}>
                <div class="evt-answer">
                  <span class="evt-label">{answerLabel}</span>
                  <Markdown text={message} />
                </div>
              </Show>
            </div>
          );
        })()}
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
        {(() => {
          const pl = payload();
          // § C — parse_recovered is a benign breadcrumb, not a failure:
          // the agent loop caught a malformed/prose reply and either
          // coerced it to Done or retried. Render it compact + muted.
          const isRecovered = pl.kind === 'parse_recovered';
          if (isRecovered) {
            const how = pl.recovered_as === 'coerced_to_done'
              ? 'reply had no JSON — accepted as final answer'
              : 'reply was not valid JSON — asked the model to retry';
            return (
              <div class="evt-recovered">
                <span class="evt-icon">↻</span>
                <span class="dim">parse recovered · {how}</span>
              </div>
            );
          }
          return (
            <div class="evt-fail">
              <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
              <pre class="evt-output">{JSON.stringify(pl, null, 2)}</pre>
            </div>
          );
        })()}
      </Show>

      <Show when={p.evt.kind === 'verdict'}>
        {(() => {
          const v = payload().verdict;
          const ok = v === 'pass' || v === 'done';
          // The verdict's `message` repeats the preceding `done` decision
          // verbatim — don't render it twice. Just show the validation
          // outcome as a compact badge row.
          return (
            <div class={`evt-verdict-row ${ok ? 'evt-ok' : 'evt-fail'}`}>
              <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
              <span class={`pill ${ok ? 'good' : 'error'}`}>{String(v)}</span>
              <span class="dim">validation {ok ? 'passed' : 'failed'}</span>
            </div>
          );
        })()}
      </Show>

      <Show when={p.evt.kind === 'heartbeat'}>
        <div class="evt-heartbeat">
          <span class="evt-icon">{kindIcon(p.evt.kind)}</span>
          <span class="fade">
            step {String(payload().step ?? '?')}
            <Show when={typeof payload().max_steps === 'number'}>
              <span class="dim">/{String(payload().max_steps)}</span>
            </Show>
          </span>
        </div>
      </Show>
      {/* `attempt` events used to render their own "step N" header, which
          duplicated the heartbeat-driven one. The model metadata they carry
          (model name, dialect) belongs on the heartbeat row instead — handled
          by the Transcript-level dedupe below. */}

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
  const filtered = createMemo(() => {
    const out: TimelineEvent[] = [];
    let lastStep: number | undefined;
    for (const e of p.events) {
      // Drop llm_chunk events — those would flood the transcript. They are
      // collapsed into surrounding decisions by the agent loop already.
      if (e.kind === 'llm_chunk') continue;
      // § C front-fix — drop `attempt` events whose step matches the most
      // recent heartbeat we already rendered. The user sees one "step N"
      // header per step, not two. We still keep an attempt event if it
      // arrives before any heartbeat (defensive — shouldn't happen in
      // practice since loop_.rs logs heartbeat before pick).
      if (e.kind === 'attempt') {
        try {
          const p = JSON.parse(e.payloadJson) as Record<string, unknown>;
          const step = typeof p.step === 'number' ? p.step : undefined;
          if (step !== undefined && step === lastStep) continue;
        } catch {
          // fall through and render
        }
      }
      if (e.kind === 'heartbeat') {
        try {
          const p = JSON.parse(e.payloadJson) as Record<string, unknown>;
          lastStep = typeof p.step === 'number' ? p.step : lastStep;
        } catch {
          // ignore
        }
      }
      out.push(e);
    }
    return out;
  });

  // Interleave conversation-turn headers: every time the task_id changes
  // from one event to the next, the user started a new follow-up turn.
  const rows = createMemo<Row[]>(() => {
    const evs = filtered();
    const out: Row[] = [];
    let lastTaskId: string | undefined;
    let turnIndex = 0;
    for (const e of evs) {
      if (e.taskId !== lastTaskId) {
        turnIndex += 1;
        out.push({
          kind: 'turn',
          taskId: e.taskId,
          goal: p.taskGoals?.[e.taskId] ?? '',
          index: turnIndex,
        });
        lastTaskId = e.taskId;
      }
      out.push({ kind: 'event', evt: e });
    }
    return out;
  });

  return (
    <div class="transcript">
      <For each={rows()}>
        {(row) =>
          row.kind === 'turn' ? (
            <div class="turn-header">
              <span class="turn-badge">turn {row.index}</span>
              <Show when={row.goal} fallback={<span class="dim">follow-up</span>}>
                <span class="turn-goal">{row.goal}</span>
              </Show>
            </div>
          ) : (
            <EventBlock
              evt={row.evt}
              selected={Number(row.evt.id) === p.selectedEvtId}
            />
          )
        }
      </For>
    </div>
  );
};

export default Transcript;
