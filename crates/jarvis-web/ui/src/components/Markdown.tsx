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

const Markdown: Component<{ text: string }> = (p) => {
  const html = createMemo(() => renderMarkdown(p.text ?? ''));
  // eslint-disable-next-line solid/no-innerhtml
  return <div class="markdown" innerHTML={html()} />;
};

export default Markdown;
