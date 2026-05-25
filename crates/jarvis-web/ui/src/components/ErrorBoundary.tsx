import {
  ErrorBoundary as SolidErrorBoundary,
  Show,
  createEffect,
  on,
  type JSX,
  type ParentComponent,
} from 'solid-js';
import { useLocation } from '@solidjs/router';

type Props = {
  /** Optional label rendered as a chip in the fallback so the user/dev
   *  knows which zone failed (e.g. "Dashboard", "Settings"). */
  name?: string;
  /** Optional custom fallback. If omitted the default UI is used. */
  fallback?: (err: unknown, reset: () => void) => JSX.Element;
};

/** Best-effort extraction of a human-readable message from an unknown thrown value. */
function describeError(err: unknown): string {
  if (err instanceof Error) return err.message || err.toString();
  if (typeof err === 'string') return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

function describeStack(err: unknown): string | null {
  if (err instanceof Error && err.stack) return err.stack;
  return null;
}

/**
 * Default fallback rendered when a child throws. Lives in its own component
 * so we can wire a `createEffect` to the route's `pathname` and auto-reset
 * when the user navigates away from a broken screen.
 */
const DefaultFallback: ParentComponent<{
  err: unknown;
  reset: () => void;
  name?: string;
}> = (props) => {
  const loc = useLocation();
  // Auto-reset whenever the route changes. The ErrorBoundary fallback only
  // mounts after a throw, so this effect captures the path AT THE TIME OF
  // THE ERROR; any subsequent pathname change calls `reset()`. We pass
  // `defer: true` so the initial mount does not immediately fire reset.
  createEffect(
    on(
      () => loc.pathname,
      () => props.reset(),
      { defer: true },
    ),
  );

  const message = () => describeError(props.err);
  const stack = () => describeStack(props.err);

  return (
    <div class="error-boundary-fallback" role="alert" aria-live="polite">
      <div class="row" style="align-items: center; gap: 0.5rem">
        <h3 class="heading" style="margin: 0">Something went wrong</h3>
        <Show when={props.name}>
          <span class="pill error">{props.name}</span>
        </Show>
      </div>
      <p class="error" style="margin: 0.5rem 0; font-size: 13px; word-break: break-word">
        {message()}
      </p>
      <Show when={import.meta.env.DEV && stack()}>
        <pre class="error-boundary-stack dim">{stack()}</pre>
      </Show>
      <div class="row" style="gap: 0.4rem; margin-top: 0.6rem">
        <button
          type="button"
          class="btn"
          onClick={() => window.location.reload()}
        >
          Reload
        </button>
        <button
          type="button"
          class="btn ghost"
          onClick={() => props.reset()}
        >
          Reset
        </button>
      </div>
    </div>
  );
};

/**
 * Lightweight wrapper around SolidJS' `<ErrorBoundary>` that:
 *  - logs the error to the console (so devtools always show it),
 *  - renders an informative fallback (message, stack in dev, reload/reset),
 *  - auto-resets when the user navigates to a different route.
 *
 * Usage:
 *   <AppErrorBoundary name="Dashboard">
 *     <DashboardContent />
 *   </AppErrorBoundary>
 */
const AppErrorBoundary: ParentComponent<Props> = (props) => {
  return (
    <SolidErrorBoundary
      fallback={(err, reset) => {
        // Surface in devtools — Solid's ErrorBoundary swallows otherwise.
        // eslint-disable-next-line no-console
        console.error('[AppErrorBoundary]', props.name ?? 'root', err);
        if (props.fallback) return props.fallback(err, reset);
        return (
          <DefaultFallback err={err} reset={reset} name={props.name} />
        );
      }}
    >
      {props.children}
    </SolidErrorBoundary>
  );
};

export default AppErrorBoundary;
