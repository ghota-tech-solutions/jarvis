// § F2.9 — task transcript / event export helpers.
//
// Two output formats:
//   - `markdown`: human-readable transcript with goal, status, per-event
//     blocks (decisions, tool calls + outputs, observations, verdicts).
//     Pasteable into a gist / Linear / wiki.
//   - `json`: raw event payloads plus task metadata. Machine-readable
//     for diffing / replaying / archival.
//
// Output is triggered via a browser `Blob` + temporary `<a>` click — no
// extra dep, works in Tauri webview the same way it does on web.

import type {
  Event as RealtimeEvent,
  Task,
  TimelineEvent,
} from '~/lib/api/gen/jarvis_pb';

// Both the realtime stream (`Event`) and the timeline snapshot
// (`TimelineEvent`) share the same on-the-wire shape; the only thing
// that differs is the type tag. Accept either so callers don't have to
// re-map.
type AnyEvent = RealtimeEvent | TimelineEvent;

function microsToIso(micros: bigint): string {
  if (!micros) return '';
  return new Date(Number(micros / 1000n)).toISOString();
}

function parsePayload(ev: AnyEvent): Record<string, unknown> | null {
  if (!ev.payloadJson) return null;
  try {
    const v = JSON.parse(ev.payloadJson);
    return typeof v === 'object' && v !== null
      ? (v as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function eventTitle(ev: AnyEvent, p: Record<string, unknown> | null): string {
  const tool = p?.tool as string | undefined;
  if (tool) return `${ev.kind} · ${tool}`;
  return ev.kind;
}

function fenceCode(s: string, lang = ''): string {
  // Use 4 backticks if the body contains a triple-fence — keeps the
  // generated md parseable even when tool output already includes fenced
  // blocks (e.g. `git diff` output, model markdown).
  const fence = s.includes('```') ? '````' : '```';
  return `${fence}${lang}\n${s.trimEnd()}\n${fence}`;
}

function eventBody(p: Record<string, unknown> | null): string {
  if (!p) return '';
  const out: string[] = [];
  if (typeof p.thought === 'string' && p.thought.trim()) {
    out.push(`**Thought:** ${p.thought.trim()}`);
  }
  if (typeof p.message === 'string' && p.message.trim()) {
    out.push(`**Message:**\n\n${p.message.trim()}`);
  }
  if (typeof p.action === 'string' && p.action.trim()) {
    out.push(`**Action:** \`${p.action}\``);
  }
  if (p.args !== undefined && p.args !== null) {
    out.push(`**Args:**\n\n${fenceCode(JSON.stringify(p.args, null, 2), 'json')}`);
  }
  if (typeof p.summary === 'string' && p.summary.trim()) {
    out.push(`**Summary:** ${p.summary.trim()}`);
  }
  if (p.data !== undefined && p.data !== null) {
    const txt =
      typeof p.data === 'string' ? p.data : JSON.stringify(p.data, null, 2);
    out.push(`**Data:**\n\n${fenceCode(txt, 'json')}`);
  }
  if (typeof p.error === 'string' && p.error.trim()) {
    out.push(`**Error:** ${p.error.trim()}`);
  }
  return out.join('\n\n');
}

export function taskToMarkdown(task: Task, events: AnyEvent[]): string {
  const lines: string[] = [];
  lines.push(`# Task ${task.id.slice(0, 8)} — ${task.goal}`);
  lines.push('');
  lines.push(`- **Status:** ${task.status}`);
  lines.push(`- **Workdir:** \`${task.workdir}\``);
  if (task.sandbox) lines.push(`- **Sandbox:** ${task.sandbox}`);
  if (task.netPolicy) lines.push(`- **Net policy:** ${task.netPolicy}`);
  if (task.createdAt) lines.push(`- **Created:** ${microsToIso(task.createdAt)}`);
  if (task.completedAt)
    lines.push(`- **Completed:** ${microsToIso(task.completedAt)}`);
  if (task.error) lines.push(`- **Error:** ${task.error}`);
  lines.push('');
  lines.push(`Exported on ${new Date().toISOString()} from the jarvis SPA.`);
  lines.push('');
  lines.push('---');
  lines.push('');
  for (const ev of events) {
    const ts = microsToIso(ev.tsMicros);
    const p = parsePayload(ev);
    lines.push(`## ${eventTitle(ev, p)} · \`#${ev.id}\` · ${ts}`);
    if (ev.subject) lines.push(`> ${ev.subject}`);
    lines.push('');
    const body = eventBody(p);
    if (body) {
      lines.push(body);
      lines.push('');
    }
  }
  return lines.join('\n');
}

export function taskToJson(task: Task, events: AnyEvent[]): string {
  return JSON.stringify(
    {
      exportedAt: new Date().toISOString(),
      task: {
        id: task.id,
        goal: task.goal,
        status: task.status,
        workdir: task.workdir,
        sandbox: task.sandbox,
        netPolicy: task.netPolicy,
        worktreePath: task.worktreePath,
        worktreeBranch: task.worktreeBranch,
        createdAt: task.createdAt?.toString() ?? null,
        completedAt: task.completedAt?.toString() ?? null,
        error: task.error ?? null,
      },
      events: events.map((ev) => ({
        id: ev.id?.toString() ?? null,
        kind: ev.kind,
        agentId: ev.agentId,
        subject: ev.subject,
        tsMicros: ev.tsMicros?.toString() ?? null,
        parentEvt: ev.parentEvt?.toString() ?? null,
        payload: parsePayload(ev),
      })),
    },
    null,
    2,
  );
}

export function downloadBlob(filename: string, content: string, mime: string) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  // Revoke async so the browser has a tick to start the download.
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
