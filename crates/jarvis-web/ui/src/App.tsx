import {
  Show,
  createEffect,
  createSignal,
  on,
  type ParentComponent,
} from 'solid-js';
import { useLocation } from '@solidjs/router';
import { getToken } from '~/lib/env';
import SidebarLeft from '~/components/SidebarLeft';
import SidebarRight from '~/components/SidebarRight';
import HelpOverlay from '~/components/HelpOverlay';
import CommandPalette from '~/components/CommandPalette';
import AppErrorBoundary from '~/components/ErrorBoundary';
import CostHud from '~/features/hud/CostHud';
import { toggleTheme, useTheme } from '~/lib/stores/theme';
import { useAppearance } from '~/lib/theme-apply';
import { useGlobalShortcuts, setHelpOpenSig } from '~/lib/stores/shortcuts';
import { useTaskNotifications } from '~/lib/stores/notifications';
import { useT } from '~/lib/i18n';

const App: ParentComponent = (props) => {
  const loc = useLocation();
  const isTaskRoute = () => loc.pathname.startsWith('/task/');
  const theme = useTheme();
  const t = useT();
  useAppearance();
  useGlobalShortcuts();
  useTaskNotifications();

  // Read token lazily from sessionStorage so we survive Vite HMR
  // (the env.ts module's bootstrap may have run on a previous instance
  // and TOKEN-as-const would be stale).
  const [hasToken, setHasToken] = createSignal(!!getToken());
  setInterval(() => setHasToken(!!getToken()), 1000);

  // Mobile: the left nav is an off-canvas drawer. Close it on any route
  // change so tapping a nav link doesn't leave the drawer covering the
  // page the user just navigated to.
  const [navOpen, setNavOpen] = createSignal(false);
  createEffect(on(() => loc.pathname, () => setNavOpen(false)));

  return (
    <div class="app">
      <header class="app-header">
        <button
          type="button"
          class="hamburger"
          onClick={() => setNavOpen((v) => !v)}
          aria-label={t().app.toggle_nav}
        >
          ☰
        </button>
        <a href="/" class="brand">{t().app.brand}</a>
        <span class="fade">·</span>
        <span class="dim">{t().app.surface}</span>
        <Show when={!hasToken()}>
          <span class="warn" style="margin-left: 1rem">
            {t().app.no_token} <code>#token=&lt;web.token&gt;</code> {t().app.no_token_hint}
          </span>
        </Show>
        <div style="margin-left: auto; display: flex; gap: 0.4rem; align-items: center">
          <button
            type="button"
            class="btn ghost"
            style="padding: 0.2rem 0.5rem; font-size: 11px"
            onClick={toggleTheme}
            title={t().app.toggle_theme}
          >
            {theme() === 'dark' ? '☾' : '☀'}
          </button>
          <button
            type="button"
            class="btn ghost"
            style="padding: 0.2rem 0.5rem; font-size: 11px"
            onClick={() => setHelpOpenSig(true)}
            title={t().app.keyboard_shortcuts}
          >
            ?
          </button>
        </div>
      </header>
      <aside class={`sidebar-left ${navOpen() ? 'drawer-open' : ''}`}>
        <SidebarLeft />
      </aside>
      <Show when={navOpen()}>
        <div
          class="drawer-backdrop"
          onClick={() => setNavOpen(false)}
          aria-hidden="true"
        />
      </Show>
      <main class="app-main">
        <AppErrorBoundary name="App">
          {props.children}
        </AppErrorBoundary>
      </main>
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
