// F1.2 — scaffold tests. We exercise the pure `renderMarkdown` HTML
// pipeline first (no DOM required, dirt cheap) and then mount the Solid
// component once to confirm the wiring is wired.

import { describe, expect, it } from 'vitest';
import { render } from '@solidjs/testing-library';
import Markdown, { renderMarkdown } from './Markdown';

describe('renderMarkdown', () => {
  it('renders headings h1-h3', () => {
    const out = renderMarkdown('# one\n\n## two\n\n### three');
    expect(out).toContain('<h1>one</h1>');
    expect(out).toContain('<h2>two</h2>');
    expect(out).toContain('<h3>three</h3>');
  });

  it('renders fenced code blocks with inline code preserved and HTML escaped', () => {
    const out = renderMarkdown('Use `cargo build` then:\n\n```rust\nfn main() {}\n```');
    // Inline code in the paragraph
    expect(out).toContain('<code>cargo build</code>');
    // Fenced code block with lang attr
    expect(out).toContain('data-lang="rust"');
    expect(out).toContain('fn main() {}');
  });

  it('escapes HTML to prevent XSS', () => {
    const out = renderMarkdown('<script>alert(1)</script>\n\nplain & text');
    expect(out).not.toContain('<script>');
    expect(out).toContain('&lt;script&gt;');
    expect(out).toContain('&amp;');
  });

  it('renders unordered and ordered lists', () => {
    const ul = renderMarkdown('- a\n- b\n- c');
    expect(ul).toContain('<ul>');
    expect(ul).toContain('<li>a</li>');
    expect(ul).toContain('<li>c</li>');

    const ol = renderMarkdown('1. first\n2. second');
    expect(ol).toContain('<ol>');
    expect(ol).toContain('<li>first</li>');
    expect(ol).toContain('<li>second</li>');
  });

  // § F2.5 — GFM extras.

  it('renders strikethrough', () => {
    const out = renderMarkdown('this is ~~deleted~~ text');
    expect(out).toContain('<del>deleted</del>');
  });

  it('renders a GFM pipe table with header + rows', () => {
    const md = ['| col1 | col2 |', '|---|---|', '| a | b |', '| c | d |'].join(
      '\n',
    );
    const out = renderMarkdown(md);
    expect(out).toContain('<table class="md-table">');
    expect(out).toContain('<th>col1</th>');
    expect(out).toContain('<th>col2</th>');
    expect(out).toContain('<td>a</td>');
    expect(out).toContain('<td>d</td>');
  });

  it('honours per-column alignment markers in tables', () => {
    const md = ['| L | C | R |', '|:---|:---:|---:|', '| a | b | c |'].join(
      '\n',
    );
    const out = renderMarkdown(md);
    expect(out).toContain('style="text-align: left"');
    expect(out).toContain('style="text-align: center"');
    expect(out).toContain('style="text-align: right"');
  });

  it('ignores pipe lines that are not followed by a separator row', () => {
    const md = 'first | line\nsecond line';
    const out = renderMarkdown(md);
    expect(out).not.toContain('<table');
  });
});

describe('<Markdown />', () => {
  it('mounts and injects the rendered HTML into a div.markdown', () => {
    const { container } = render(() => <Markdown text="# hello" />);
    const root = container.querySelector('.markdown');
    expect(root).not.toBeNull();
    expect(root!.querySelector('h1')?.textContent).toBe('hello');
  });
});
