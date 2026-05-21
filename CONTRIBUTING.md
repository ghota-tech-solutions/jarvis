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

### Local LLM setup

Jarvis needs an OpenAI-compatible LLM endpoint. The recommended setup — omlx
on Apple Silicon, the two Gemma 4 model variants, omlx tuning (VLM MTP, hot
cache, chunked prefill), and the `.env` wiring — lives in the README's
[**Local LLM setup**](./README.md#local-llm-setup) section, since it is
end-user configuration rather than contributor-only.

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

By contributing, you agree that your contributions will be licensed under
the MIT License, matching the project license.

## Questions?

Open a [Discussion](https://github.com/ghota-tech-solutions/jarvis/discussions)
or an issue tagged `question`. We try to respond within a few days.
