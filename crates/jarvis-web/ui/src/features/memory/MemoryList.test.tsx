// F1.2 — MemoryList rendering + promote/forget actions.
//
// We bypass createQuery's fetcher by seeding the QueryClient cache up
// front (`setQueryData` before render). The component then reads from
// cache synchronously and `q.data` is populated on the first render —
// no Solid resource round-trip, no Suspense gymnastics. Mutations
// (promote / forget) still get asserted via their mocks, which is what
// we actually care about.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor } from '@solidjs/testing-library';
import { QueryClient, QueryClientProvider } from '@tanstack/solid-query';
import type { Memory, MemoryList as MemoryListMsg } from '~/lib/api/gen/jarvis_pb';

const promoteMemoryMock = vi.fn();
const forgetMemoryMock = vi.fn();
const editMemoryMock = vi.fn();
const listMemoriesMock = vi.fn();

vi.mock('~/lib/api/client', () => ({
  jarvis: {
    listMemories: (...a: unknown[]) => listMemoriesMock(...a),
    promoteMemory: (...a: unknown[]) => promoteMemoryMock(...a),
    forgetMemory: (...a: unknown[]) => forgetMemoryMock(...a),
    editMemory: (...a: unknown[]) => editMemoryMock(...a),
  },
}));

// Import AFTER vi.mock so the mock is in place when the module evaluates.
import MemoryList from './MemoryList';

const memory = (over: Partial<Memory>): Memory =>
  ({
    $typeName: 'jarvis.v1.Memory',
    id: 1n,
    scope: 'global',
    scopeValue: '',
    kind: 'pattern',
    text: 'always run cargo fmt before commit',
    status: 'candidate',
    sourceTaskId: '',
    createdAtMicros: 0n,
    updatedAtMicros: 0n,
    usageCount: 0,
    ...over,
  }) as Memory;

const makeList = (memories: Memory[]): MemoryListMsg =>
  ({
    $typeName: 'jarvis.v1.MemoryList',
    memories,
  }) as MemoryListMsg;

const renderList = (memories: Memory[]) => {
  const qc = new QueryClient({
    defaultOptions: {
      queries: { retry: false, refetchOnWindowFocus: false, staleTime: Infinity },
    },
  });
  // The queryKey in MemoryList is ['memories', p.workdirFilter ?? null].
  // We render without a workdirFilter prop, so the key is ['memories', null].
  qc.setQueryData(['memories', null], makeList(memories));
  return render(() => (
    <QueryClientProvider client={qc}>
      <MemoryList />
    </QueryClientProvider>
  ));
};

beforeEach(() => {
  listMemoriesMock.mockReset().mockResolvedValue(makeList([]));
  promoteMemoryMock.mockReset().mockResolvedValue({});
  forgetMemoryMock.mockReset().mockResolvedValue({});
  editMemoryMock.mockReset().mockResolvedValue({});
});

afterEach(() => cleanup());

describe('<MemoryList />', () => {
  it('renders text + kind pill for both candidate and active memories', () => {
    renderList([
      memory({ id: 1n, kind: 'pattern', status: 'candidate', text: 'candidate text' }),
      memory({ id: 2n, kind: 'fact', status: 'active', text: 'active text' }),
    ]);

    expect(screen.getByText('candidate text')).toBeInTheDocument();
    expect(screen.getByText('active text')).toBeInTheDocument();
    // Kind pills present
    expect(screen.getByText('pattern')).toBeInTheDocument();
    expect(screen.getByText('fact')).toBeInTheDocument();
  });

  it('calls jarvis.promoteMemory(id) when "promote" is clicked on a candidate', async () => {
    renderList([memory({ id: 42n, status: 'candidate' })]);

    const btn = screen.getByRole('button', { name: /^promote$/i });
    fireEvent.click(btn);

    await waitFor(() => {
      expect(promoteMemoryMock).toHaveBeenCalledTimes(1);
    });
    expect(promoteMemoryMock).toHaveBeenCalledWith({ id: 42n, text: '' });
  });

  it('calls jarvis.forgetMemory(id) when "forget" is clicked on an active memory', async () => {
    renderList([memory({ id: 77n, status: 'active' })]);

    const btn = screen.getByRole('button', { name: /^forget$/i });
    fireEvent.click(btn);

    await waitFor(() => {
      expect(forgetMemoryMock).toHaveBeenCalledTimes(1);
    });
    expect(forgetMemoryMock).toHaveBeenCalledWith({ id: 77n });
  });
});
