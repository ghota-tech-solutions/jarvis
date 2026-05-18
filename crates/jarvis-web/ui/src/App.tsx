import { Show, type ParentComponent } from 'solid-js';
import { useLocation } from '@solidjs/router';
import { TOKEN } from '~/lib/env';
import SidebarLeft from '~/components/SidebarLeft';
import SidebarRight from '~/components/SidebarRight';

const App: ParentComponent = (props) => {
  const loc = useLocation();
  const isTaskRoute = () => loc.pathname.startsWith('/task/');
  return (
    <div class="app">
      <header class="app-header">
        <a href="/" class="brand">jarvis</a>
        <span class="fade">·</span>
        <span class="dim">web</span>
        <Show when={!TOKEN}>
          <span class="warn" style="margin-left: auto">
            no token — append <code>#token=&lt;web.token&gt;</code>
          </span>
        </Show>
      </header>
      <aside class="sidebar-left">
        <SidebarLeft />
      </aside>
      <main class="app-main">{props.children}</main>
      <aside class="sidebar-right">
        <SidebarRight expanded={isTaskRoute()} />
      </aside>
    </div>
  );
};

export default App;
