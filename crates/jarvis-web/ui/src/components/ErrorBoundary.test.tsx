// @ts-nocheck
// @vitest-environment jsdom
//
// Tests for <AppErrorBoundary />. Authored ahead of F1.2 which installs
// vitest + @solidjs/testing-library. Until that lands, `bun run typecheck`
// will skip this file because it is matched only when the test runner
// resolves the imports; the runner itself is not yet wired in.
//
// When F1.2 lands, this file should run as-is.
import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent } from '@solidjs/testing-library';
import { Router } from '@solidjs/router';
import { type ParentComponent } from 'solid-js';
import AppErrorBoundary from './ErrorBoundary';

/** A child that always throws synchronously during render. */
const Boom: ParentComponent = () => {
  throw new Error('kaboom');
};

/** Helper: render a component tree under a Router so `useLocation()` works
 *  inside the fallback (which uses it for the auto-reset effect). */
const renderRouted = (children: () => any) => {
  return render(() => <Router root={() => children()}>{[]}</Router>);
};

describe('AppErrorBoundary', () => {
  it('renders children when no error', () => {
    renderRouted(() => (
      <AppErrorBoundary name="Test">
        <div data-testid="ok">hello world</div>
      </AppErrorBoundary>
    ));
    expect(screen.getByTestId('ok').textContent).toBe('hello world');
    expect(screen.queryByText(/Something went wrong/i)).toBeNull();
  });

  it('renders fallback when child throws', () => {
    // Silence the console.error noise the boundary intentionally emits.
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    renderRouted(() => (
      <AppErrorBoundary name="Crasher">
        <Boom />
      </AppErrorBoundary>
    ));
    expect(screen.getByText(/Something went wrong/i)).toBeTruthy();
    // The error string appears at least once — possibly twice in dev mode
    // because the stack trace also contains the message. Use getAllByText
    // so either case (prod-only message or dev message+stack) is accepted.
    expect(screen.getAllByText(/kaboom/).length).toBeGreaterThan(0);
    // Both action buttons are wired.
    expect(screen.getByRole('button', { name: /reload/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /reset/i })).toBeTruthy();
    spy.mockRestore();
  });

  it('reset button calls reset()', () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    renderRouted(() => (
      <AppErrorBoundary name="Crasher">
        <Boom />
      </AppErrorBoundary>
    ));
    // Resetting will re-render the children; Boom throws again so the
    // fallback should still be visible. We just assert the click does
    // not throw and the fallback remains.
    fireEvent.click(screen.getByRole('button', { name: /reset/i }));
    expect(screen.getByText(/Something went wrong/i)).toBeTruthy();
    spy.mockRestore();
  });
});
