import { For, Show, createEffect, createMemo, on, type Component } from 'solid-js';
import type { TimelineEvent } from '~/lib/api/gen/jarvis_pb';

type Props = {
  events: TimelineEvent[];
};

type TerminalEntry = {
  id: number;
  tool: string;
  args: string;
  timestamp: string;
  output?: string;
  ok?: boolean;
  exit?: number;
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
  
  // Find the command or target argument to display cleanly
  for (const key of ['cmd', 'path', 'pattern', 'file', 'target']) {
    const v = args[key];
    if (typeof v === 'string') return { tool, args: v };
  }
  
  // Specific tool tweaks
  if (tool === 'replace_file_content' || tool === 'multi_replace_file_content') {
    const file = args.TargetFile || args.Target;
    if (typeof file === 'string') return { tool, args: file.split('/').pop() || file };
  }
  
  return { tool, args: JSON.stringify(args) };
};

const toolResultSummary = (p: Record<string, unknown>): {
  ok: boolean;
  exit?: number;
  output: string;
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
          : p.error
            ? String(p.error)
            : JSON.stringify(p);
            
  return { ok, exit, output };
};

const TerminalLogs: Component<Props> = (p) => {
  let scrollerRef: HTMLDivElement | undefined;

  const entries = createMemo<TerminalEntry[]>(() => {
    const map = new Map<number, TerminalEntry>();
    const list: TerminalEntry[] = [];
    
    for (const e of p.events) {
      if (e.kind === 'tool_call') {
        const payload = parsePayload(e.payloadJson);
        const s = toolCallSummary(payload);
        const entry: TerminalEntry = {
          id: Number(e.id),
          tool: s.tool,
          args: s.args,
          timestamp: new Date(Number(e.tsMicros) / 1000).toLocaleTimeString(),
        };
        map.set(Number(e.id), entry);
        list.push(entry);
      } else if (e.kind === 'tool_result') {
        const parentId = Number(e.parentEvt);
        const entry = map.get(parentId);
        if (entry) {
          const payload = parsePayload(e.payloadJson);
          const s = toolResultSummary(payload);
          entry.output = s.output;
          entry.ok = s.ok;
          entry.exit = s.exit;
        }
      }
    }
    return list;
  });

  // Auto-scroll to bottom of terminal logs as new commands execute
  createEffect(
    on(
      () => entries().length,
      () => {
        if (!scrollerRef) return;
        queueMicrotask(() => {
          if (scrollerRef) {
            scrollerRef.scrollTop = scrollerRef.scrollHeight;
          }
        });
      },
    ),
  );

  return (
    <div class="terminal-box">
      <div class="terminal-header">
        <span class="terminal-dot red" />
        <span class="terminal-dot yellow" />
        <span class="terminal-dot green" />
        <span class="terminal-title">bash · active agent session logs</span>
      </div>
      
      <div class="terminal-logs" ref={scrollerRef}>
        <Show when={entries().length > 0} fallback={
          <div class="dim" style="font-style: italic; padding: 1rem 0">
            Waiting for tool calls... Command executions will stream here.
          </div>
        }>
          <For each={entries()}>
            {(entry) => (
              <div class="terminal-line">
                <div>
                  <span class="fade" style="font-size: 10px; margin-right: 0.5rem">
                    [{entry.timestamp}]
                  </span>
                  <span style="color: #55ff55; margin-right: 0.4rem">$</span>
                  <span class="terminal-cmd">{entry.tool}</span>{' '}
                  <span class="terminal-args">{entry.args}</span>
                </div>
                
                <Show when={entry.output !== undefined} fallback={
                  <div class="dim" style="margin-left: 0.8rem; font-style: italic; margin-top: 0.15rem">
                    Executing...
                  </div>
                }>
                  <pre class="terminal-output">{entry.output}</pre>
                  <div style="margin-left: 0.8rem; font-size: 11px; margin-top: 0.2rem">
                    <span class="fade">status: </span>
                    <Show when={entry.ok} fallback={
                      <span class="terminal-error">
                        FAILED (exit {entry.exit !== undefined ? entry.exit : 'err'})
                      </span>
                    }>
                      <span class="terminal-success">
                        SUCCESS (exit {entry.exit !== undefined ? entry.exit : 0})
                      </span>
                    </Show>
                  </div>
                </Show>
              </div>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
};

export default TerminalLogs;
