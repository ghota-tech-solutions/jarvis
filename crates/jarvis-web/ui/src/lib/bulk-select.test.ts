// § F1.7 — tests for createBulkSelect primitive.

import { describe, expect, it } from 'vitest';
import { createBulkSelect } from './bulk-select';

describe('createBulkSelect', () => {
  it('starts empty', () => {
    const b = createBulkSelect<string>();
    expect(b.count()).toBe(0);
    expect(b.ids()).toEqual([]);
    expect(b.isSelected('x')).toBe(false);
  });

  it('toggle adds then removes', () => {
    const b = createBulkSelect<string>();
    b.toggle('a');
    expect(b.isSelected('a')).toBe(true);
    expect(b.count()).toBe(1);
    b.toggle('a');
    expect(b.isSelected('a')).toBe(false);
    expect(b.count()).toBe(0);
  });

  it('selectAll replaces selection', () => {
    const b = createBulkSelect<string>();
    b.toggle('x');
    b.selectAll(['a', 'b', 'c']);
    expect(b.count()).toBe(3);
    expect(b.isSelected('x')).toBe(false);
    expect(b.isSelected('a')).toBe(true);
    expect(b.isSelected('b')).toBe(true);
    expect(b.isSelected('c')).toBe(true);
  });

  it('clear empties everything', () => {
    const b = createBulkSelect<string>();
    b.selectAll(['a', 'b']);
    b.clear();
    expect(b.count()).toBe(0);
    expect(b.ids()).toEqual([]);
  });

  it('works with bigint ids (memory id case)', () => {
    const b = createBulkSelect<bigint>();
    b.toggle(42n);
    b.toggle(99n);
    expect(b.count()).toBe(2);
    expect(b.isSelected(42n)).toBe(true);
    expect(b.isSelected(7n)).toBe(false);
    b.toggle(42n);
    expect(b.isSelected(42n)).toBe(false);
    expect(b.count()).toBe(1);
  });
});
