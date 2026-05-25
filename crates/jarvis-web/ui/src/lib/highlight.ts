// § F1.8 — Shiki-backed syntax highlighting for diff text.
//
// Trade-off for v1: we highlight unified-diff text with Shiki's bundled
// `diff` grammar. That gives proper colors for +/-/@@/--- lines via the
// chosen theme, with zero per-language work. The downside is that the
// *underlying* file language (Rust, TS, Python, …) is NOT syntax-coloured
// inside the changed lines — they appear as plain monospace text styled
// by the +/- color. A future iteration can compose two passes (first
// language, then diff overlay) for richer output, but that's a meaningful
// rewrite and out of scope here.
//
// Highlighter and the diff grammar are loaded lazily on first call so the
// initial app bundle isn't paying for Shiki at boot.

import type { BundledLanguage, Highlighter } from 'shiki';

const LIGHT_THEME = 'github-light';
const DARK_THEME = 'github-dark';

let highlighterPromise: Promise<Highlighter> | null = null;
const loadedLangs = new Set<BundledLanguage>();

/** Shared Shiki highlighter — created once, lazily. Exposed so other
 *  callers (e.g. `<Markdown>` code-block enhancer) reuse the same
 *  runtime instance instead of spinning up a second one. */
export async function getHighlighter(): Promise<Highlighter> {
  if (!highlighterPromise) {
    const { createHighlighter } = await import('shiki');
    highlighterPromise = createHighlighter({
      themes: [LIGHT_THEME, DARK_THEME],
      langs: [], // load on demand
    });
  }
  return highlighterPromise;
}

/** Lazy-load a language grammar onto the shared highlighter. Idempotent. */
export async function ensureLang(
  h: Highlighter,
  lang: BundledLanguage,
): Promise<void> {
  if (loadedLangs.has(lang)) return;
  await h.loadLanguage(lang);
  loadedLangs.add(lang);
}

export const SHIKI_THEMES = { light: LIGHT_THEME, dark: DARK_THEME } as const;

/** Maps a file path's extension to a Shiki bundled language. Returns null
 *  when the extension is unknown — caller should fall back to plain text. */
const EXT_TO_LANG: Record<string, BundledLanguage> = {
  rs: 'rust',
  ts: 'typescript',
  tsx: 'tsx',
  js: 'javascript',
  jsx: 'jsx',
  mjs: 'javascript',
  cjs: 'javascript',
  py: 'python',
  go: 'go',
  java: 'java',
  kt: 'kotlin',
  rb: 'ruby',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  swift: 'swift',
  json: 'json',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'toml',
  md: 'markdown',
  mdx: 'mdx',
  sql: 'sql',
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  ps1: 'powershell',
  proto: 'proto',
  css: 'css',
  scss: 'scss',
  html: 'html',
  xml: 'xml',
  svg: 'xml',
  vue: 'vue',
  graphql: 'graphql',
  gql: 'graphql',
  dockerfile: 'docker',
  lock: 'toml', // best guess for *.lock files we encounter
};

export function langForPath(path: string): BundledLanguage | null {
  const lower = path.toLowerCase();
  // Dockerfile (no extension).
  if (lower.endsWith('dockerfile') || lower.endsWith('/dockerfile')) {
    return 'docker';
  }
  const ext = lower.split('.').pop();
  if (!ext) return null;
  return EXT_TO_LANG[ext] ?? null;
}

/** Highlight a unified-diff text fragment into HTML using Shiki's bundled
 *  `diff` grammar. Returns the HTML string (a `<pre><code>…</code></pre>`
 *  structure already styled with Shiki tokens) or `null` on failure (the
 *  caller should fall back to plain rendering). */
export async function highlightDiff(diffText: string): Promise<string | null> {
  if (!diffText) return null;
  try {
    const h = await getHighlighter();
    await ensureLang(h, 'diff' as BundledLanguage);
    return h.codeToHtml(diffText, {
      lang: 'diff',
      themes: { light: LIGHT_THEME, dark: DARK_THEME },
      defaultColor: false, // emit CSS variables so we can pick at render time
    });
  } catch {
    return null;
  }
}
