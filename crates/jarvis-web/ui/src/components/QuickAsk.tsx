// Quick Ask widget — one-shot streaming chat with the daemon's `Ask` RPC.
// Lives on the Dashboard for top-of-mind "I just want a quick answer"
// questions that don't need a full agent loop. The model is the daemon's
// configured ask provider (highest-priority local).

import { Show, createSignal, type Component } from 'solid-js';
import { jarvis } from '~/lib/api/client';
import Markdown from './Markdown';

const QuickAsk: Component = () => {
  const [prompt, setPrompt] = createSignal('');
  const [answer, setAnswer] = createSignal('');
  const [running, setRunning] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const onSubmit = async (ev: Event) => {
    ev.preventDefault();
    const text = prompt().trim();
    if (!text || running()) return;
    setAnswer('');
    setError(null);
    setRunning(true);
    try {
      const it = jarvis.ask({
        prompt: text,
        provider: '',
        temperature: undefined,
        maxTokens: undefined,
      });
      for await (const chunk of it) {
        if (chunk.delta) setAnswer((a) => a + chunk.delta);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  const onKey = (ev: KeyboardEvent) => {
    if (ev.key === 'Enter' && !ev.shiftKey) {
      ev.preventDefault();
      onSubmit(ev);
    }
  };

  return (
    <section class="quick-ask">
      <header class="quick-ask-head">
        <h3 class="section-title" style="margin: 0">Quick Ask</h3>
        <span class="dim" style="font-size: 11px">
          one-shot chat — no tools, no workdir
        </span>
      </header>
      <form class="quick-ask-form" onSubmit={onSubmit}>
        <textarea
          class="textarea"
          placeholder="Ask anything — Enter sends, Shift+Enter newlines"
          value={prompt()}
          onInput={(e) => setPrompt(e.currentTarget.value)}
          onKeyDown={onKey}
          rows={2}
          disabled={running()}
        />
        <button
          type="submit"
          class="btn"
          disabled={running() || !prompt().trim()}
          style="align-self: flex-start"
        >
          {running() ? 'streaming…' : 'ask'}
        </button>
      </form>
      <Show when={answer() || error()}>
        <div class="quick-ask-answer">
          <Show when={error()} fallback={<Markdown text={answer()} />}>
            <span class="error">{error()}</span>
          </Show>
        </div>
      </Show>
    </section>
  );
};

export default QuickAsk;
