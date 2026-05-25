import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@solidjs/testing-library';
import { EmptyState } from './EmptyState';

describe('EmptyState', () => {
  it('renders title with default illustration, no hint or action', () => {
    const { container, queryByRole } = render(() => (
      <EmptyState title="Nothing here" />
    ));

    expect(container.querySelector('.empty-state-title')?.textContent).toBe(
      'Nothing here',
    );
    // Default SVG illustration is mounted.
    expect(container.querySelector('.empty-state-illustration svg')).not.toBeNull();
    // No hint paragraph.
    expect(container.querySelector('.empty-state-hint')).toBeNull();
    // No action button or link.
    expect(queryByRole('button')).toBeNull();
    expect(container.querySelector('a.empty-state-action')).toBeNull();
  });

  it('renders title + hint together', () => {
    const { container } = render(() => (
      <EmptyState title="No tasks" hint="Submit one below." />
    ));

    expect(container.querySelector('.empty-state-title')?.textContent).toBe(
      'No tasks',
    );
    expect(container.querySelector('.empty-state-hint')?.textContent).toBe(
      'Submit one below.',
    );
  });

  it('renders a button that invokes onAction when actionLabel + onAction are provided', () => {
    const spy = vi.fn();
    const { getByRole, container } = render(() => (
      <EmptyState
        title="No tasks"
        actionLabel="Create one"
        onAction={spy}
      />
    ));

    const btn = getByRole('button', { name: 'Create one' });
    expect(btn.tagName).toBe('BUTTON');
    expect(container.querySelector('a.empty-state-action')).toBeNull();

    fireEvent.click(btn);
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it('renders an <a> with the given href when actionLabel + actionHref are provided, no <button>', () => {
    const { container, queryByRole } = render(() => (
      <EmptyState
        title="No tasks"
        actionLabel="Go home"
        actionHref="/"
      />
    ));

    const link = container.querySelector('a.empty-state-action') as HTMLAnchorElement | null;
    expect(link).not.toBeNull();
    expect(link!.getAttribute('href')).toBe('/');
    expect(link!.textContent).toBe('Go home');
    // Crucially: no button — href wins.
    expect(queryByRole('button')).toBeNull();
  });
});
