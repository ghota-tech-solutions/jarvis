// § F2.9 — tests for the task transcript exporters.

import { describe, expect, it } from 'vitest';
import { taskToJson, taskToMarkdown } from './export';
import type { Event as ApiEvent, Task } from '~/lib/api/gen/jarvis_pb';

const makeTask = (overrides: Partial<Task> = {}): Task =>
  ({
    $typeName: 'jarvis.v1.Task',
    id: 'aabbccdd-1234-5678-9abc-def012345678',
    goal: 'Refactor src/main.rs to use anyhow',
    status: 'completed',
    workdir: '/tmp/work',
    sandbox: 'native',
    netPolicy: 'egress_only',
    worktreePath: '',
    worktreeBranch: '',
    createdAt: 1_700_000_000_000_000n,
    completedAt: 1_700_000_300_000_000n,
    error: '',
    tokensIn: 0n,
    tokensOut: 0n,
    estimatedCostUsd: 0,
    createdAtMicros: 0n,
    updatedAtMicros: 0n,
    needsAttention: false,
    ...overrides,
  }) as unknown as Task;

const makeEvent = (overrides: Partial<ApiEvent> = {}): ApiEvent =>
  ({
    $typeName: 'jarvis.v1.Event',
    id: 1n,
    tsMicros: 1_700_000_010_000_000n,
    taskId: 'aabbccdd-1234-5678-9abc-def012345678',
    agentId: '',
    kind: 'decision',
    subject: '',
    payloadJson: JSON.stringify({
      action: 'tool',
      tool: 'fs_read',
      args: { path: 'src/main.rs' },
      thought: 'I need to see the file first.',
    }),
    parentEvt: 0n,
    ...overrides,
  }) as unknown as ApiEvent;

describe('taskToMarkdown', () => {
  it('renders the task header with goal and status', () => {
    const md = taskToMarkdown(makeTask(), []);
    expect(md).toContain('# Task aabbccdd — Refactor src/main.rs to use anyhow');
    expect(md).toContain('**Status:** completed');
    expect(md).toContain('**Sandbox:** native');
  });

  it('renders an event block with title, thought, action, args', () => {
    const md = taskToMarkdown(makeTask(), [makeEvent()]);
    expect(md).toContain('## decision · fs_read · `#1`');
    expect(md).toContain('**Thought:** I need to see the file first.');
    expect(md).toContain('**Action:** `tool`');
    expect(md).toContain('"path": "src/main.rs"');
  });

  it('escapes triple-backtick output by switching to four-fence', () => {
    const ev = makeEvent({
      payloadJson: JSON.stringify({
        summary: 'patch applied',
        data: '```rust\nfn main() {}\n```',
      }),
    });
    const md = taskToMarkdown(makeTask(), [ev]);
    expect(md).toContain('````');
  });
});

describe('taskToJson', () => {
  it('roundtrips through JSON.parse', () => {
    const json = taskToJson(makeTask(), [makeEvent()]);
    const parsed = JSON.parse(json);
    expect(parsed.task.id).toBe('aabbccdd-1234-5678-9abc-def012345678');
    expect(parsed.task.status).toBe('completed');
    expect(parsed.events).toHaveLength(1);
    expect(parsed.events[0].kind).toBe('decision');
    expect(parsed.events[0].payload.tool).toBe('fs_read');
  });

  it('serialises bigint ids as decimal strings', () => {
    const json = taskToJson(makeTask(), [makeEvent({ id: 42n })]);
    const parsed = JSON.parse(json);
    expect(parsed.events[0].id).toBe('42');
  });
});
