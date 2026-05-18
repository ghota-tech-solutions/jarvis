// === Smart-follow auto-scroll =================================================
// The page (window) scrolls now — not the events container — so we follow on
// window.scrollY. We track "following" state: when the user has scrolled near
// the bottom we keep snapping; if they scroll up we pause and show a floating
// "↓ N new" button to jump back.
(function setupSmartFollow() {
  const SLACK = 80;
  const jumpBtn = document.getElementById('jump-bottom');
  const jumpCount = document.getElementById('jump-count');
  const isAtBottom = () => (window.innerHeight + window.scrollY) >= (document.documentElement.scrollHeight - SLACK);
  let following = true;
  let unseen = 0;
  function refreshJump() {
    if (!jumpBtn) return;
    if (!following && unseen > 0) {
      jumpCount.textContent = unseen + ' new';
      jumpBtn.classList.add('show');
    } else {
      jumpBtn.classList.remove('show');
    }
  }
  function snapBottom() { window.scrollTo({top: document.documentElement.scrollHeight, behavior: 'instant'}); }
  window.addEventListener('scroll', () => {
    following = isAtBottom();
    if (following) { unseen = 0; refreshJump(); }
  }, {passive: true});
  jumpBtn?.addEventListener('click', () => {
    following = true;
    unseen = 0;
    snapBottom();
    refreshJump();
  });
  window.__jarvisFollow = {
    onNew() { if (following) snapBottom(); else { unseen++; refreshJump(); } },
    isFollowing() { return following; },
  };
  // Initial scroll-to-bottom on load.
  window.addEventListener('load', () => snapBottom());
})();

// === Streaming composer + thinking timer ======================================
(function setupStreamingComposer() {
  const composing = document.getElementById('composing');
  if (!composing) return;
  const events = document.getElementById('events');
  if (!events) return;

  let thinkingEl = null;
  let thinkingTimer = null;
  const THINKING_DELAY = 1500;

  function showThinking() {
    if (thinkingEl || !composing.hidden) return;
    thinkingEl = document.createElement('div');
    thinkingEl.className = 'turn thinking';
    thinkingEl.innerHTML = '<span class="dots"><span></span><span></span><span></span></span><span>thinking…</span>';
    composing.parentNode.insertBefore(thinkingEl, composing);
    window.__jarvisFollow?.onNew();
  }
  function clearThinking() {
    if (thinkingTimer) { clearTimeout(thinkingTimer); thinkingTimer = null; }
    if (thinkingEl) { thinkingEl.remove(); thinkingEl = null; }
  }
  function armThinking() {
    clearThinking();
    thinkingTimer = setTimeout(showThinking, THINKING_DELAY);
  }

  function fadeOutComposing() {
    composing.classList.add('fade-out');
    setTimeout(() => {
      composing.hidden = true;
      composing.textContent = '';
      composing.classList.remove('fade-out');
    }, 220);
  }

  function tryAttach() {
    const es = events.__sse?.source || htmx.find(events)?.__sse?.source;
    if (!es || es._jarvis_attached) {
      setTimeout(tryAttach, 100);
      return;
    }
    es._jarvis_attached = true;
    es.addEventListener('open', () => armThinking());
    es.addEventListener('chunk', (ev) => {
      clearThinking();
      if (composing.hidden) { composing.hidden = false; composing.textContent = ''; composing.classList.remove('fade-out'); }
      composing.textContent += ev.data;
      window.__jarvisFollow?.onNew();
    });
    es.addEventListener('decision', (ev) => {
      clearThinking();
      fadeOutComposing();
      // Insert the rendered decision block AFTER the composing element.
      events.insertAdjacentHTML('beforeend', ev.data);
      window.__jarvisFollow?.onNew();
      armThinking();
    });
    es.addEventListener('event', () => {
      // Any non-chunk event resets the thinking timer.
      clearThinking();
      armThinking();
      window.__jarvisFollow?.onNew();
    });
  }
  tryAttach();
})();

// === Auto-scroll on HTMX swaps (covers initial backfill + sse swaps) ==========
document.body.addEventListener('htmx:afterSwap', () => {
  window.__jarvisFollow?.onNew();
});

// === Continue form: refocus + clear textarea after submit =====================
document.body.addEventListener('htmx:afterRequest', (e) => {
  if (e.target.id === 'continue' && e.detail.successful) {
    const ta = e.target.querySelector('textarea');
    if (ta) { ta.value = ''; ta.style.height = 'auto'; ta.focus(); }
  }
});

// === Auto-resize textareas as you type ========================================
function autoResize(ta) {
  ta.style.height = 'auto';
  const next = Math.min(ta.scrollHeight, 300);
  ta.style.height = next + 'px';
}
document.querySelectorAll('textarea[name="goal"]').forEach((ta) => {
  ta.addEventListener('input', () => autoResize(ta));
  ta.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
      e.preventDefault();
      if (typeof ta.form.requestSubmit === 'function') ta.form.requestSubmit();
      else ta.form.submit();
    }
  });
});

// === "+ Continue" link in the left nav: focus + scroll the textarea ===========
document.querySelectorAll('a[href="#continue"]').forEach((a) => {
  a.addEventListener('click', (e) => {
    e.preventDefault();
    const ta = document.querySelector('#continue textarea');
    if (ta) { ta.focus(); ta.scrollIntoView({behavior: 'smooth', block: 'end'}); }
  });
});

// === Auto-focus the continue textarea on page load ============================
window.addEventListener('load', () => {
  document.querySelector('#continue textarea')?.focus();
});

// === Inline "show N more" for action-out + diff-body wrappers =================
// The button lives inside .output-wrap.clipped. Clicking removes the clip so
// the same block grows; no separate sub-card is spawned.
document.addEventListener('click', (e) => {
  const btn = e.target.closest('.show-more-btn');
  if (!btn) return;
  const wrap = btn.closest('.output-wrap');
  if (wrap) {
    wrap.classList.remove('clipped');
    window.__jarvisFollow?.onNew();
  }
});

// === Git commit modal + PR opener =============================================
function jarvisCloseCommitModal() {
  document.getElementById('git-commit-modal').classList.remove('open');
}
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') jarvisCloseCommitModal();
});
async function jarvisOpenPr(workdir) {
  try {
    const r = await fetch('/api/git/pr-url?workdir=' + encodeURIComponent(workdir));
    const j = await r.json();
    if (j.url) window.open(j.url, '_blank', 'noopener');
    else alert('No origin remote on GitHub — cannot construct PR URL.');
  } catch (err) { alert('PR URL fetch failed: ' + err); }
}

// Generate a commit message from the diff using the LLM.
async function jarvisSuggestCommitMessage() {
  const wd = document.getElementById('git-commit-workdir').value;
  const ta = document.getElementById('git-commit-message');
  const status = document.getElementById('git-commit-status');
  const btn = document.querySelector('#git-commit-modal .suggest-btn');
  const stageAll = document.getElementById('git-commit-stage-all')?.checked ?? true;
  if (!wd) { status.textContent = 'no workdir'; return; }
  if (btn) { btn.disabled = true; btn.textContent = '✨ thinking…'; }
  status.textContent = '';
  try {
    const scope = stageAll ? 'all' : 'staged';
    const r = await fetch('/api/git/suggest-message?workdir=' + encodeURIComponent(wd) + '&scope=' + scope);
    if (!r.ok) { status.textContent = 'error: ' + (await r.text()); }
    else {
      const j = await r.json();
      ta.value = j.message || '';
      ta.dispatchEvent(new Event('input'));
      status.innerHTML = '<span class="muted small">via ' + (j.model || 'llm') + '</span>';
    }
  } catch (err) { status.textContent = 'failed: ' + err; }
  finally { if (btn) { btn.disabled = false; btn.textContent = '✨ Suggest'; } }
}

// Confirm + run a git reset.
async function jarvisGitReset(workdir, mode) {
  const verb = mode === 'hard' ? 'DISCARD ALL UNCOMMITTED CHANGES (hard reset)' : 'unstage all changes (mixed reset)';
  if (!confirm('git reset --' + mode + ' HEAD — ' + verb + '? This cannot be undone.')) return;
  const fd = new FormData();
  fd.append('workdir', workdir);
  fd.append('mode', mode);
  const r = await fetch('/api/git/reset', { method: 'POST', body: fd });
  if (r.ok && window.htmx) {
    const card = document.getElementById('git-card');
    if (card) window.htmx.trigger(card, 'load');
  } else if (!r.ok) { alert('reset failed: ' + (await r.text())); }
}

// Continue-button: pushes a continuation follow-up using the existing compose
// form so the agent picks up where it left off, no matter the verdict.
function jarvisContinue() {
  const form = document.getElementById('continue');
  const ta = form?.querySelector('textarea[name="goal"]');
  if (!form || !ta) return;
  ta.value = 'Continue working on the original goal. Identify the next concrete step from the prior turns and execute it. Do not declare done unless every requirement from the original goal is verifiably met — check the actual files, command outputs, or tests, not your own assertions.';
  if (typeof form.requestSubmit === 'function') form.requestSubmit();
  else form.submit();
}

// HTMX trigger: after a successful commit, refresh the git card immediately.
document.body.addEventListener('jarvis-git-committed', () => {
  jarvisCloseCommitModal();
  const card = document.getElementById('git-card');
  if (card && window.htmx) window.htmx.trigger(card, 'load');
});

// File click → open the diff in the main column (fullscreen-style panel).
// Escape returns to the conversation view without losing scroll position.
function jarvisOpenFullDiff(workdir, path) {
  const panel = document.getElementById('diff-fullscreen');
  const body = document.getElementById('diff-fullscreen-body');
  const title = document.getElementById('diff-fullscreen-title');
  const events = document.getElementById('events');
  if (!panel || !body) return;
  title.textContent = path || 'Diff preview';
  body.innerHTML = '<div class="muted small">loading…</div>';
  panel.hidden = false;
  if (events) events.classList.add('hidden-by-overlay');
  fetch('/api/git/diff?workdir=' + encodeURIComponent(workdir) + '&path=' + encodeURIComponent(path) + '&scope=all')
    .then(r => r.text())
    .then(html => { body.innerHTML = html || '<div class="muted small">no diff</div>'; body.scrollTop = 0; })
    .catch(err => { body.innerHTML = '<div class="err">failed: ' + err + '</div>'; });
}
function jarvisCloseFullDiff() {
  const panel = document.getElementById('diff-fullscreen');
  const events = document.getElementById('events');
  if (panel) panel.hidden = true;
  if (events) events.classList.remove('hidden-by-overlay');
}
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    const panel = document.getElementById('diff-fullscreen');
    if (panel && !panel.hidden) { jarvisCloseFullDiff(); e.preventDefault(); }
  }
});
document.body.addEventListener('click', (e) => {
  const row = e.target.closest('.git-row-file[data-path]');
  if (!row) return;
  e.preventDefault();
  const path = row.getAttribute('data-path');
  const wd = document.querySelector('#git-card .git-actions')?.getAttribute('data-workdir') || '';
  jarvisOpenFullDiff(wd, path);
});
