// § F1.6 — sanity tests on the translation dictionaries.
// The shape parity is enforced at compile time via the `Dict` type;
// these tests catch runtime regressions (missing keys at the bottom
// level, accidental empty strings, locale roundtrip).

import { describe, expect, it } from 'vitest';
import { DICTS, SUPPORTED_LOCALES, en, fr } from './dict';

const allLeafPaths = (obj: Record<string, unknown>, prefix = ''): string[] => {
  const out: string[] = [];
  for (const [k, v] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === 'object') {
      out.push(...allLeafPaths(v as Record<string, unknown>, path));
    } else {
      out.push(path);
    }
  }
  return out;
};

describe('translation dicts', () => {
  it('every supported locale has a dict registered', () => {
    for (const { code } of SUPPORTED_LOCALES) {
      expect(DICTS[code]).toBeDefined();
    }
  });

  it('en and fr expose the same leaf paths', () => {
    const enPaths = new Set(allLeafPaths(en as unknown as Record<string, unknown>));
    const frPaths = new Set(allLeafPaths(fr as unknown as Record<string, unknown>));
    expect(enPaths.size).toBe(frPaths.size);
    for (const p of enPaths) expect(frPaths.has(p)).toBe(true);
  });

  it('no leaf is an empty string', () => {
    for (const dict of Object.values(DICTS)) {
      for (const p of allLeafPaths(dict as unknown as Record<string, unknown>)) {
        const path = p.split('.');
        let cur: unknown = dict;
        for (const seg of path) cur = (cur as Record<string, unknown>)[seg];
        expect(typeof cur).toBe('string');
        expect((cur as string).length).toBeGreaterThan(0);
      }
    }
  });
});
