import type { ParentComponent } from 'solid-js';
import { A } from '@solidjs/router';
import { TOKEN } from '~/lib/env';

const App: ParentComponent = (props) => {
  return (
    <div class="app">
      <header class="app-header">
        <A href="/" class="brand">
          jarvis
        </A>
        <span class="dim">·</span>
        <span class="dim">web</span>
        {!TOKEN && (
          <span class="warn" style="margin-left: auto">
            no token — append #token=&lt;web.token&gt; to the URL
          </span>
        )}
      </header>
      <main class="app-main">{props.children}</main>
    </div>
  );
};

export default App;
