import { Show, createSignal, type ParentComponent } from 'solid-js';
import { useLocation } from '@solidjs/router';
import { getToken } from '~/lib/env';
import SidebarLeft from '~/components/SidebarLeft';
import SidebarRight from '~/components/SidebarRight';
import HelpOverlay from '~/components/HelpOverlay';
import CommandPalette from '~/components/CommandPalette';
import CostHud from '~/features/hud/CostHud';
import { bindThemeToDom, toggleTheme, useTheme } from '~/lib/stores/theme';
import { useGlobalShortcuts, setHelpOpenSig } from '~/lib/stores/shortcuts';
import { useTaskNotifications } from '~/lib/stores/notifications';

const App: ParentComponent = (props) => {
  const loc = useLocation();
  const isTaskRoute = () => loc.pathname.startsWith('/task/');
  const theme = useTheme();
  bindThemeToDom();
  useGlobalShortcuts();
  useTaskNotifications();

  // Read token lazily from sessionStorage so we survive Vite HMR
  // (the env.ts module's bootstrap may have run on a previous instance
  // and TOKEN-as-const would be stale).
  const [hasToken, setHasToken] = createSignal(!!getToken());
  setInterval(() => setHasToken(!!getToken()), 1000);

  return (
    <div class="app">
      <header class="app-header">
        <a href="/" class="brand">jarvis</a>
        <span class="fade">·</span>
        <span class="dim">web</span>
        <Show when={!hasToken()}>
          <span class="warn" style="margin-left: 1rem">
            no token — append <code>#token=&lt;web.token&gt;</code>
          </span>
        </Show>
        <div style="margin-left: auto; display: flex; gap: 0.4rem; align-items: center">
          <button
            type="button"
            class="btn ghost"
            style="padding: 0.2rem 0.5rem; font-size: 11px"
            onClick={toggleTheme}
            title="Toggle theme (t)"
          >
            {theme() === 'dark' ? '☾' : '☀'}
          </button>
          <button
            type="button"
            class="btn ghost"
            style="padding: 0.2rem 0.5rem; font-size: 11px"
            onClick={() => setHelpOpenSig(true)}
            title="Keyboard shortcuts (?)"
          >
            ?
          </button>
        </div>
      </header>
      <aside class="sidebar-left">
        <SidebarLeft />
      </aside>
      <main class="app-main">{props.children}</main>
      <aside class="sidebar-right">
        <SidebarRight expanded={isTaskRoute()} />
      </aside>
      <CostHud />
      <HelpOverlay />
      <CommandPalette />
    </div>
  );
};

export default App;
