# Contributing to Jarvis

Thanks for your interest in contributing! Jarvis is an early-stage open-source
project and we're glad to have you. This document explains how to get a working
dev environment, what kind of contributions we're looking for, and the
expectations around commits, tests, and review.

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](./CODE_OF_CONDUCT.md).
By participating, you agree to uphold its terms.

## How to contribute

We welcome:

- **Bug reports** — open an issue with reproduction steps; the smaller the
  repro, the faster the fix.
- **Feature requests** — open an issue first to discuss the design before
  writing code, especially for anything cross-crate.
- **Pull requests** — small, focused PRs land fastest. If you're about to
  invest more than a couple of hours, ping us on an issue first.
- **Documentation fixes** — README, code comments, examples; all welcome.
- **New tools / MCP integrations** — the `jarvis-tools` crate has a clean
  `Tool` trait; new tools are usually a single self-contained file.

## Dev environment

### Prerequisites

- **Rust 1.95+** (edition 2024) — `rustup toolchain install stable`
- **Bun 1.3+** for the SolidJS SPA — https://bun.sh
- `protoc` — vendored via `protoc-bin-vendored`, no install needed
- Optional: **Docker** for the containerized sandbox, **WSL2** on Windows
- For desktop work: `cargo install tauri-cli --version "^2.0"`

### Build & run

```bash
# 1. Clone and build the Rust workspace
git clone https://github.com/ghota-tech-solutions/jarvis
cd jarvis
cargo build --release --workspace

# 2. First-time config
cp .env.example .env                 # fill provider URLs / keys
cp jarvis.toml.example jarvis.toml   # tweak models, sandbox, routing

# 3. Start the daemon (shell 1)
./target/release/jarvis-daemon run

# 4. In shell 2 — your choice of client
./target/release/jarvis-tui          # ratatui TUI
./target/release/jarvis ping         # scriptable CLI

# 5. SPA dev server (optional, shell 3)
cd crates/jarvis-web/ui
bun install
bun run dev
# Open the URL Vite prints, append #token=<contents of .jarvis/web.token>
```

### Local LLM setup (macOS, Apple Silicon)

Jarvis talks to any OpenAI-compatible endpoint, so vLLM, llama.cpp, Ollama,
and LM Studio all work. The fastest path on Apple Silicon is
[**omlx**](https://github.com/jundot/omlx) — an MLX-backed server with a
built-in admin UI for model downloads and tuning.

```bash
# 1. Install
brew tap jundot/omlx
brew install --head omlx

# 2. Start the server (default port 8000)
omlx serve
```

Open the admin UI and pull the two Gemma 4 model variants we recommend for
Jarvis:

1. **Models →
   [Downloader](http://localhost:8000/admin/dashboard?tab=models&modelsTab=downloader)**
   — download both:
   - `gemma-4-26B-A4B-it-assistant-bf16` (full precision, for thinking/planning)
   - `gemma-4-26B-A4B-it-MLX-4bit` (4-bit quant, for fast tool-call turns)

2. **Settings →
   [Models](http://localhost:8000/admin/dashboard?tab=settings&settingsTab=models)**
   — open the gear on `gemma-4-26B-A4B-it-assistant-bf16` and enable:
   - **TurboQuant KV Cache** at 4-bit (cuts cache memory ~4×)
   - **deflash** (faster attention on Apple Silicon)

3. **Smoke-test** the chat at
   [/admin/chat](http://localhost:8000/admin/chat) before wiring Jarvis.

Once chat works, point Jarvis at omlx via `.env`:

```bash
# Local LLM (OpenAI-compatible endpoint — omlx, MLX-LM, llama.cpp, vLLM)
HERMES_LOCAL_URL=http://127.0.0.1:8000/v1
HERMES_LOCAL_MODEL=gemma-4-26B-A4B-it-MLX-4bit
HERMES_LOCAL_API_KEY=azer

# Remote LLM (DeepSeek API — optional, only if you want failover)
#DEEPSEEK_API_KEY=sk-xxx

# Daemon — bind 0.0.0.0 so LAN devices (e.g. mobile testing) can reach
# the daemon's gRPC + gRPC-Web endpoint. Auth bearer token gates access.
JARVIS_DAEMON_ADDR=0.0.0.0:7777
# JARVIS_SPA_ADDR overrides jarvis-web's bind. Default is 0.0.0.0:7879.
JARVIS_LOG=jarvis=debug,info

# Web search (M11.S2) — Brave Search API (free tier:
# https://api.search.brave.com/app/dashboard)
# Note: var name avoids the JARVIS_ prefix because that namespace is reserved
# for jarvis.toml field overrides (figment strict mode).
#WEB_SEARCH_BACKEND=brave
#BRAVE_API_KEY=xxx
#TAVILY_API_KEY=tvly-xxxxxxxxxxxxxxxxxxxxxxxx
```

The `HERMES_LOCAL_API_KEY` value can be anything — omlx accepts any
non-empty key. The OpenAI SDK we use requires the field to be set.

> **Linux / Windows**: omlx is macOS-only. On Linux use
> [vLLM](https://github.com/vllm-project/vllm) or
> [llama.cpp](https://github.com/ggerganov/llama.cpp); on Windows use WSL2
> with either, or [LM Studio](https://lmstudio.ai/). Any OpenAI-compatible
> server works — only `HERMES_LOCAL_URL` and `HERMES_LOCAL_MODEL` change.

## Workflow

1. **Fork** the repo and create a feature branch from `main`:
   `git checkout -b feat/short-description`
2. **Code** the change. Keep diffs small and focused.
3. **Test** locally (see below).
4. **Commit** using Conventional Commits.
5. **Open a PR** against `main` — the template will prompt you for context.

### Commit conventions

We use [Conventional Commits](https://www.conventionalcommits.org/). The type
prefix matters for the future changelog generator:

```
<type>(<scope>): <short imperative summary>

<optional body explaining the WHY>

<optional footer: BREAKING CHANGE, Closes #123, etc.>
```

Common types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `chore`,
`ci`, `build`. Scope examples: `agent`, `ledger`, `sandbox`, `ui`, `cli`,
`tui`, `web`, `tools`, `mcp`.

Examples:
- `feat(tools): add gh.pr_review tool`
- `fix(sandbox): worktree cleanup on agent crash`
- `docs(readme): clarify token discovery on Windows`

### Tests

Before opening a PR, run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib

cd crates/jarvis-web/ui && bun run typecheck && bun run build
```

CI runs the same checks on Linux + Windows. PRs with red CI won't merge.

### Adding a new tool

The `Tool` trait lives in `crates/jarvis-tools/src/lib.rs`. A new tool is
typically:

1. A new file in `crates/jarvis-tools/src/` implementing `Tool`.
2. Registration in the tool registry constructor.
3. A unit test (each existing tool has at least one).
4. A line in the README's tool list.

Keep tools side-effect-isolated: they get a `ToolContext` with sandbox +
workdir, and should never touch `process::env` directly.

### Adding a new model provider

The `LlmProvider` trait lives in `crates/jarvis-llm/src/provider.rs`. The
existing OpenAI-compatible client covers most APIs (OpenAI, DeepSeek, local
llama.cpp/Ollama). For non-OpenAI shapes, implement the trait and wire it
through `LlmPool`.

## Architecture pointers

- **DAG**: `core` is the leaf crate, no I/O. Clients (`cli`, `tui`) depend
  only on `api` + `core`. Don't add cross-crate cycles.
- **Ledger**: append-only, no `UPDATE`/`DELETE` (enforced by triggers). The
  one mutable table is `memories` (M9).
- **gRPC contract**: edits to `crates/jarvis-api/proto/jarvis.proto` are
  the canonical place. Both the Rust server and the TS client regenerate
  from it.
- **Sandbox**: three backends (`native`, `docker`, `wsl2`). New backends
  implement the `Sandbox` trait in `crates/jarvis-sandbox`.

## Security disclosure

Please **don't** open public issues for security vulnerabilities. See
[SECURITY.md](./SECURITY.md) for the responsible-disclosure process.

## Licensing

By contributing, you agree that your contributions will be dual-licensed
under MIT and Apache-2.0, matching the project license.

## Questions?

Open a [Discussion](https://github.com/ghota-tech-solutions/jarvis/discussions)
or an issue tagged `question`. We try to respond within a few days.
