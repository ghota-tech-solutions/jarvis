import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

export default defineConfig({
  plugins: [solid()],
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy: {
      // gRPC-Web calls go straight to the daemon; the proxy keeps them
      // same-origin in dev so we don't need CORS on the tonic-web layer.
      // Path prefix matches the proto package: jarvis.v1.Jarvis/<Method>.
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
