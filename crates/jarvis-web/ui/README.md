# jarvis-web/ui

SolidJS SPA. Talks to the daemon over gRPC-Web at `127.0.0.1:7777` and to
the SPA host (auth, future SSE) at `127.0.0.1:7879`. Bundled into the
`jarvis-web` Rust crate via `rust-embed` for production builds.

## Dev

```powershell
bun install
bun run dev
# open http://127.0.0.1:5173/#token=<paste from .jarvis/web.token>
```

The hash `#token=...` is parsed on first load and persisted into
`sessionStorage` (see `src/lib/env.ts`).

## Build

```powershell
bun run build
```

Output goes to `dist/`. The Rust build script in `crates/jarvis-web/build.rs`
(planned, M6.S9+) embeds it into the binary.
