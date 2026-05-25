/// <reference types="vitest" />
import { defineConfig } from 'vitest/config';
import solid from 'vite-plugin-solid';

// happy-dom over jsdom: ~3-5× faster startup, sufficient for our test set
// (we only need innerHTML, basic DOM events, and Solid's reconciler). If
// future tests need full layout APIs (getBoundingClientRect with real
// styles, getComputedStyle, etc.), switch the environment to 'jsdom'.
//
// `hot: false` disables solid-refresh, which Vitest cannot resolve
// (`file:///@solid-refresh` virtual id). `ssr: false` forces the browser
// JSX build so happy-dom can render normally.
export default defineConfig({
  plugins: [solid({ hot: false, ssr: false })],
  test: {
    environment: 'happy-dom',
    globals: true,
    setupFiles: ['./vitest.setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
  },
  resolve: {
    conditions: ['browser'],
    alias: {
      '~': new URL('./src', import.meta.url).pathname,
    },
    // Without dedupe, both the SSR/dev build and the browser build of
    // solid-js end up in the graph and you get the dreaded
    // "multiple instances of Solid" warning + dead reactivity.
    dedupe: ['solid-js', 'solid-js/web', '@solidjs/router'],
  },
});
