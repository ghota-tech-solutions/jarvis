// § F1.8 — tests for langForPath + highlightDiff fallback.
// The Shiki highlighter itself is async + heavy; we don't unit-test it.
// We assert that the lang detection mapping is correct and that
// highlightDiff returns null for empty input (the fallback path).

import { describe, expect, it } from 'vitest';
import { langForPath, highlightDiff } from './highlight';

describe('langForPath', () => {
  it('maps common extensions to bundled grammars', () => {
    expect(langForPath('src/main.rs')).toBe('rust');
    expect(langForPath('app/index.ts')).toBe('typescript');
    expect(langForPath('App.tsx')).toBe('tsx');
    expect(langForPath('config.yml')).toBe('yaml');
    expect(langForPath('config.yaml')).toBe('yaml');
    expect(langForPath('manifest.toml')).toBe('toml');
    expect(langForPath('SCRIPT.SH')).toBe('bash');
    expect(langForPath('schema.proto')).toBe('proto');
  });

  it('recognises Dockerfile without extension', () => {
    expect(langForPath('Dockerfile')).toBe('docker');
    expect(langForPath('build/Dockerfile')).toBe('docker');
  });

  it('returns null for unknown extensions and pathless inputs', () => {
    expect(langForPath('unknown.xyz')).toBeNull();
    expect(langForPath('README')).toBeNull();
    expect(langForPath('')).toBeNull();
  });
});

describe('highlightDiff', () => {
  it('returns null for empty input without touching Shiki', async () => {
    expect(await highlightDiff('')).toBeNull();
  });
});
