// § C front-fix + F2.5 — markdown renderer for LLM-emitted text.
//
// No external dependency for the *parser*: keeps the SPA under the 2 MB
// bundle cap and dodges the XSS-via-third-party-parser surface. Handles
// the subset the agent actually emits: headers, paragraphs, bullets,
// ordered lists, inline / fenced code, bold, italic, strikethrough,
// links, horizontal rule, GFM-style pipe tables, plus our custom
// step-divider / reasoning / answer sections.
//
// The input is HTML-escaped before any markdown rule runs, so the only
// HTML in the output comes from our own template strings — no raw user
// HTML leaks through.
//
// § F2.5 enhancement: code blocks are lazy-syntax-highlighted via Shiki
// (loaded on demand by the component, not the pure renderer). The
// renderer emits `<pre class="md-code" data-lang="...">` and the
// component walks those nodes after mount to swap them for highlighted
// HTML — falls back to plain-text rendering when Shiki is loading or
// errors, so the user always sees their code.

import { createEffect, createMemo, type Component } from 'solid-js';

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
  // Strikethrough: ~~text~~  (GFM).
  out = out.replace(/~~([^~\n]+)~~/g, '<del>$1</del>');
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

export const renderMarkdown = (md: string): string => {
  const lines = md.split(/\r?\n/);
  let html = '';
  let inCode = false;
  let codeLang = '';
  let codeBody: string[] = [];
  let inList = false;
  let listType: 'ul' | 'ol' | null = null;
  let inPara = false;
  let paraBody: string[] = [];
  let activeSection: 'reasoning' | 'answer' | null = null;

  const flushPara = () => {
    if (inPara) {
      if (paraBody.length > 0) {
        html += `<p>${paraBody.join('<br />')}</p>\n`;
      }
      inPara = false;
      paraBody = [];
    }
  };

  const flushList = () => {
    if (inList && listType) {
      html += `</${listType}>\n`;
      inList = false;
      listType = null;
    }
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    // 1. Code Blocks
    if (line.trim().startsWith('```')) {
      if (inCode) {
        // End of code block
        const escapedBody = escapeHtml(codeBody.join('\n'));
        html += `<pre class="md-code"${codeLang ? ` data-lang="${codeLang}"` : ''}><code>${escapedBody}</code></pre>\n`;
        inCode = false;
        codeBody = [];
        codeLang = '';
      } else {
        // Start of code block
        flushPara();
        flushList();
        inCode = true;
        codeLang = line.trim().slice(3).trim();
      }
      continue;
    }

    if (inCode) {
      codeBody.push(line);
      continue;
    }

    // 2. Blank Lines
    if (line.trim() === '') {
      flushPara();
      flushList();
      continue;
    }

    // 2.5 Box-drawing step divider (e.g. ─step 3/20 or ─ step 3 ─)
    const stepMatch = line.match(/^\s*(?:─+|-+)\s*step\s*(\d+(?:\/\d+)?)(?:\s*(?:─+|-+)?\s*)?$/i);
    if (stepMatch) {
      flushPara();
      flushList();
      if (activeSection !== null) {
        html += `</div>\n</div>\n`;
        activeSection = null;
      }
      html += `<div class="md-step-divider"><span class="md-step-icon">─</span><span class="md-step-text">step ${stepMatch[1]}</span></div>\n`;
      continue;
    }

    // 2.6 Reasoning title (case-insensitive reasoning/thought/thinking)
    const reasoningMatch = line.match(/^\s*(reasoning|thought|thinking)\s*:?\s*$/i);
    if (reasoningMatch) {
      flushPara();
      flushList();
      if (activeSection !== null) {
        html += `</div>\n</div>\n`;
      }
      activeSection = 'reasoning';
      html += `<div class="md-reasoning-section">\n<div class="md-section-title md-reasoning-title"><span class="md-section-icon">💭</span> reasoning</div>\n<div class="md-reasoning-content">\n`;
      continue;
    }

    // 2.7 Answer title (case-insensitive answer/message/response)
    const answerMatch = line.match(/^\s*(answer|message|response)\s*:?\s*$/i);
    if (answerMatch) {
      flushPara();
      flushList();
      if (activeSection !== null) {
        html += `</div>\n</div>\n`;
      }
      activeSection = 'answer';
      html += `<div class="md-answer-section">\n<div class="md-section-title md-answer-title"><span class="md-section-icon">🎯</span> answer</div>\n<div class="md-answer-content">\n`;
      continue;
    }


    // 3. Headings
    const headingMatch = line.match(/^\s*(#{1,6})\s+(.+)$/);
    if (headingMatch) {
      flushPara();
      flushList();
      const level = headingMatch[1].length;
      const content = renderInline(escapeHtml(headingMatch[2]));
      html += `<h${level}>${content}</h${level}>\n`;
      continue;
    }

    // 3.5 § F2.5 — GFM pipe tables.
    // Header row `| col | col |` immediately followed by a separator
    // `|---|---|` (optional `:` for alignment). Subsequent `| ... |`
    // rows become tbody. Stops at the first non-pipe line.
    if (line.trim().startsWith('|') && i + 1 < lines.length) {
      const sep = lines[i + 1].trim();
      const looksLikeSep = /^\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?$/.test(sep);
      if (looksLikeSep) {
        flushPara();
        flushList();
        const splitRow = (l: string): string[] =>
          l
            .trim()
            .replace(/^\||\|$/g, '')
            .split('|')
            .map((c) => c.trim());
        const aligns = splitRow(sep).map((cell) => {
          const left = cell.startsWith(':');
          const right = cell.endsWith(':');
          if (left && right) return 'center';
          if (right) return 'right';
          if (left) return 'left';
          return null;
        });
        const headerCells = splitRow(line);
        html += '<table class="md-table">\n<thead>\n<tr>';
        for (let c = 0; c < headerCells.length; c++) {
          const a = aligns[c];
          const alignAttr = a ? ` style="text-align: ${a}"` : '';
          html += `<th${alignAttr}>${renderInline(escapeHtml(headerCells[c]))}</th>`;
        }
        html += '</tr>\n</thead>\n<tbody>\n';
        let j = i + 2;
        while (j < lines.length && lines[j].trim().startsWith('|')) {
          const row = splitRow(lines[j]);
          html += '<tr>';
          for (let c = 0; c < row.length; c++) {
            const a = aligns[c];
            const alignAttr = a ? ` style="text-align: ${a}"` : '';
            html += `<td${alignAttr}>${renderInline(escapeHtml(row[c]))}</td>`;
          }
          html += '</tr>\n';
          j++;
        }
        html += '</tbody>\n</table>\n';
        i = j - 1; // outer for-loop will i++; skip to the line after the table
        continue;
      }
    }

    // 4. Horizontal Rule
    if (/^\s*(-{3,}|_{3,}|\*{3,})$/.test(line.trim())) {
      flushPara();
      flushList();
      html += '<hr />\n';
      continue;
    }

    // 5. Blockquotes
    const quoteMatch = line.match(/^\s*>\s+(.+)$/);
    if (quoteMatch) {
      flushPara();
      flushList();
      html += `<blockquote>${renderInline(escapeHtml(quoteMatch[1]))}</blockquote>\n`;
      continue;
    }

    // 6. Bullet Lists
    const bulletMatch = line.match(/^\s*[-*+]\s+(.+)$/);
    if (bulletMatch) {
      flushPara();
      if (!inList || listType !== 'ul') {
        flushList();
        inList = true;
        listType = 'ul';
        html += '<ul>\n';
      }
      html += `<li>${renderInline(escapeHtml(bulletMatch[1]))}</li>\n`;
      continue;
    }

    // 7. Ordered Lists
    const orderedMatch = line.match(/^\s*(\d+)\.\s+(.+)$/);
    if (orderedMatch) {
      flushPara();
      if (!inList || listType !== 'ol') {
        flushList();
        inList = true;
        listType = 'ol';
        html += '<ol>\n';
      }
      html += `<li>${renderInline(escapeHtml(orderedMatch[2]))}</li>\n`;
      continue;
    }

    // 8. Paragraph or continuous text
    flushList();
    if (!inPara) {
      inPara = true;
    }
    paraBody.push(renderInline(escapeHtml(line)));
  }

  flushPara();
  flushList();

  if (activeSection !== null) {
    html += `</div>\n</div>\n`;
  }

  return html;
};

/** § F2.5 — post-mount Shiki highlight pass for `pre.md-code[data-lang]`.
 *
 *  We don't run Shiki inside `renderMarkdown` (the pure function stays
 *  dep-free + dirt-cheap to test). Instead the component walks its own
 *  DOM after each render and progressively replaces unhighlighted code
 *  blocks with Shiki output. Failures fall back silently to the plain
 *  escaped text already on the page. */
async function highlightCodeBlocks(root: HTMLElement) {
  const nodes = root.querySelectorAll(
    'pre.md-code[data-lang]:not([data-shiki])',
  );
  if (nodes.length === 0) return;
  try {
    const { getHighlighter, ensureLang, SHIKI_THEMES } = await import(
      '~/lib/highlight'
    );
    const h = await getHighlighter();
    for (const node of Array.from(nodes)) {
      const el = node as HTMLElement;
      const lang = el.dataset.lang || '';
      const codeEl = node.querySelector('code');
      if (!codeEl) continue;
      const src = codeEl.textContent ?? '';
      try {
        await ensureLang(h, lang as never);
      } catch {
        el.dataset.shiki = 'skipped'; // unknown grammar; keep plain
        continue;
      }
      try {
        const out = h.codeToHtml(src, {
          lang,
          themes: SHIKI_THEMES,
          defaultColor: false,
        });
        el.innerHTML = out;
        el.dataset.shiki = 'on';
      } catch {
        el.dataset.shiki = 'failed';
      }
    }
  } catch {
    // Shiki itself failed to load — keep plain rendering.
  }
}

const Markdown: Component<{ text: string }> = (p) => {
  const html = createMemo(() => renderMarkdown(p.text ?? ''));
  let rootEl: HTMLDivElement | undefined;
  createEffect(() => {
    // Re-run highlight whenever the rendered HTML changes.
    html();
    if (rootEl) void highlightCodeBlocks(rootEl);
  });
  return (
    <div
      class="markdown"
      ref={(el) => (rootEl = el)}
      // eslint-disable-next-line solid/no-innerhtml
      innerHTML={html()}
    />
  );
};

export default Markdown;
