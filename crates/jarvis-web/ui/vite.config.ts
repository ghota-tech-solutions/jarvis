import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

export default defineConfig({
  plugins: [solid()],
  server: {
    // Bind on all interfaces so phones / other devices on the LAN can
    // open the SPA at http://<host-lan-ip>:5173.
    host: '0.0.0.0',
    port: 5173,
    strictPort: true,
    proxy: {
      // gRPC-Web calls go through the proxy in dev so the browser stays
      // same-origin (no CORS on the daemon's tonic-web layer needed).
      '/jarvis.v1.Jarvis': {
        target: 'http://127.0.0.1:7777',
        changeOrigin: true,
      },
    },
  },
  build: {
    target: 'es2022',
    outDir: 'dist',
    sourcemap: true,
  },
  resolve: {
    alias: {
      '~': '/src',
    },
  },
});
