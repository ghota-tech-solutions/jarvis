// § C front-fix — minimal markdown renderer for LLM-emitted text.
//
// No external dependency: keeps the SPA under the 2 MB bundle cap and
// dodges the XSS-via-third-party-parser surface. Handles the subset the
// agent actually emits: headers, paragraphs, bullets, ordered lists,
// inline / fenced code, bold, italic, links, hr.
//
// The input is HTML-escaped before any markdown rule runs, so the only
// HTML in the output comes from our own template strings — no raw user
// HTML leaks through. Use this component for any text that originated
// from the model (decision.thought, decision.message, verdict.message,
// tool_result.output when we know the tool produces markdown).

import { createMemo, type Component } from 'solid-js';

const escapeHtml = (s: string): string =>
  s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');

// Inline transformations: bold, italic, inline code, links. Applied to
// already-escaped HTML, so the &-encoded < / > pass through unchanged.
const renderInline = (s: string): string => {
  let out = s;
  // Inline code first so its content isn't mangled by * / _ rules.
  out = out.replace(/`([^`\n]+)`/g, '<code>$1</code>');
  // Bold: **text** or __text__
  out = out.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>');
  out = out.replace(/__([^_\n]+)__/g, '<strong>$1</strong>');
  // Italic: *text* or _text_ (but not the middle of bold; the bold rule
  // already consumed those).
  out = out.replace(/(^|[^*])\*([^*\n]+)\*/g, '$1<em>$2</em>');
  out = out.replace(/(^|[^_])_([^_\n]+)_/g, '$1<em>$2</em>');
  // Links: [text](url) — only safe schemes (http/https/mailto).
  out = out.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, text, url) => {
    const safe = /^(https?:|mailto:)/.test(url);
    return safe
      ? `<a href="${url}" target="_blank" rel="noopener noreferrer">${text}</a>`
      : text;
  });
  return out;
};

const renderBlocks = (escaped: string): string => {
  // Split into blocks on blank lines.
  const blocks = escaped.split(/\n{2,}/);
  const out: string[] = [];
  for (const blkRaw of blocks) {
    const blk = blkRaw.replace(/\s+$/, '');
    if (!blk) continue;

    // Fenced code block: ```lang\n...\n```
    const fenceMatch = blk.match(/^```(\w*)\n([\s\S]*?)\n```$/);
    if (fenceMatch) {
      const lang = fenceMatch[1] || '';
      const body = fenceMatch[2];
      out.push(
        `<pre class="md-code"${lang ? ` data-lang="${lang}"` : ''}><code>${body}</code></pre>`
      );
      continue;
    }

    // Heading: # / ## / ### / #### / ##### / ######
    const headingMatch = blk.match(/^(#{1,6})\s+(.+)$/);
    if (headingMatch && !blk.includes('\n')) {
      const level = headingMatch[1].length;
      out.push(`<h${level}>${renderInline(headingMatch[2])}</h${level}>`);
      continue;
    }

    // Horizontal rule
    if (/^(-{3,}|_{3,}|\*{3,})$/.test(blk)) {
      out.push('<hr />');
      continue;
    }

    // Bulleted list (all lines start with `- ` or `* `)
    const lines = blk.split('\n');
    if (lines.every((l) => /^[-*]\s+/.test(l))) {
      const items = lines
        .map((l) => `<li>${renderInline(l.replace(/^[-*]\s+/, ''))}</li>`)
        .join('');
      out.push(`<ul>${items}</ul>`);
      continue;
    }
    // Ordered list (all lines start with `<number>. `)
    if (lines.every((l) => /^\d+\.\s+/.test(l))) {
      const items = lines
        .map((l) => `<li>${renderInline(l.replace(/^\d+\.\s+/, ''))}</li>`)
        .join('');
      out.push(`<ol>${items}</ol>`);
      continue;
    }

    // Default: paragraph. Convert single newlines to <br>.
    const para = lines.map(renderInline).join('<br />');
    out.push(`<p>${para}</p>`);
  }
  return out.join('\n');
};

export const renderMarkdown = (md: string): string => {
  const escaped = escapeHtml(md);
  return renderBlocks(escaped);
};

const Markdown: Component<{ text: string }> = (p) => {
  const html = createMemo(() => renderMarkdown(p.text ?? ''));
  // eslint-disable-next-line solid/no-innerhtml
  return <div class="markdown" innerHTML={html()} />;
};

export default Markdown;
