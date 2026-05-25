// F1.2 — CommandPalette behaviour tests.
//
// The palette is wired to a module-level signal (`open`), so each test
// closes it via the exported `closePalette` helper in afterEach to keep
// state from bleeding between cases.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, fireEvent, cleanup, screen } from '@solidjs/testing-library';
import { MemoryRouter, Route } from '@solidjs/router';
import { QueryClient, QueryClientProvider } from '@tanstack/solid-query';
import type { ParentComponent } from 'solid-js';
import CommandPalette, { closePalette, openPalette } from './CommandPalette';

// Stub the daemon client — none of these tests actually hit RPCs (they
// only navigate and toggle theme), but the module is imported at the top
// of CommandPalette so it must resolve without touching the network.
vi.mock('~/lib/api/client', () => ({
  jarvis: {
    submitTask: vi.fn(),
  },
}));

const renderPalette = () => {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  // Mount the palette inside MemoryRouter's `root`. The Route catches
  // the initial "/" location and renders nothing — the palette itself is
  // not route-bound; it just needs the routing context (useNavigate).
  const Root: ParentComponent = (p) => (
    <QueryClientProvider client={qc}>
      <CommandPalette />
      <div data-testid="route-outlet">{p.children}</div>
    </QueryClientProvider>
  );
  return render(() => (
    <MemoryRouter root={Root}>
      <Route path="/" component={() => <span />} />
    </MemoryRouter>
  ));
};

beforeEach(() => {
  closePalette();
});

afterEach(() => {
  closePalette();
  cleanup();
});

describe('<CommandPalette />', () => {
  it('opens when Ctrl+K is pressed on the window', async () => {
    renderPalette();
    // Closed initially.
    expect(screen.queryByPlaceholderText(/type a command/i)).toBeNull();

    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });

    // Solid renders synchronously after the signal flip.
    expect(screen.getByPlaceholderText(/type a command/i)).toBeInTheDocument();
  });

  it('closes when Escape is pressed', () => {
    renderPalette();
    openPalette();
    const input = screen.getByPlaceholderText(/type a command/i) as HTMLInputElement;
    expect(input).toBeInTheDocument();

    fireEvent.keyDown(input, { key: 'Escape' });
    expect(screen.queryByPlaceholderText(/type a command/i)).toBeNull();
  });

  it('filters commands as the user types', () => {
    renderPalette();
    openPalette();
    const input = screen.getByPlaceholderText(/type a command/i) as HTMLInputElement;

    // No query → multiple entries visible.
    expect(screen.getByText('Go to Dashboard')).toBeInTheDocument();
    expect(screen.getByText('Toggle theme dark/light')).toBeInTheDocument();

    fireEvent.input(input, { target: { value: 'theme' } });

    // 'theme' substring keeps the toggle-theme entry, drops the others.
    expect(screen.getByText('Toggle theme dark/light')).toBeInTheDocument();
    expect(screen.queryByText('Go to Dashboard')).toBeNull();
  });

  it('runs the selected command on Enter (closes after a no-arg command)', async () => {
    renderPalette();
    openPalette();
    const input = screen.getByPlaceholderText(/type a command/i) as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'theme' } });
    expect(screen.getByText('Toggle theme dark/light')).toBeInTheDocument();

    fireEvent.keyDown(input, { key: 'Enter' });

    // toggle-theme has no argLabel → the palette runs the cb and closes
    // synchronously (the run callback resolves immediately).
    await Promise.resolve();
    await Promise.resolve();
    expect(screen.queryByPlaceholderText(/type a command/i)).toBeNull();
  });
});
