// § F1.3 — tests for Skeleton primitives. Authored as TODO by the F1.3
// worktree (vitest wasn't installed in that branch); added here now that
// vitest is in place.

import { describe, expect, it } from 'vitest';
import { render } from '@solidjs/testing-library';
import { Skeleton, SkeletonCard, SkeletonList } from './Skeleton';

describe('Skeleton', () => {
  it('renders a span with the skeleton class and aria-busy', () => {
    const { container } = render(() => <Skeleton />);
    const el = container.querySelector('.skeleton');
    expect(el).toBeTruthy();
    expect(el?.getAttribute('aria-busy')).toBe('true');
  });

  it('SkeletonCard with N lines renders N child skeletons', () => {
    const { container } = render(() => <SkeletonCard lines={4} />);
    const skeletons = container.querySelectorAll('.skeleton-card > .skeleton');
    expect(skeletons.length).toBe(4);
  });

  it('SkeletonList renders the requested number of cards', () => {
    const { container } = render(() => <SkeletonList count={5} />);
    const cards = container.querySelectorAll('.skeleton-list > .skeleton-card');
    expect(cards.length).toBe(5);
  });
});
