# Jarvis

Autonomous multi-agent coding system, daemon + clients (CLI, TUI, Web), provider-agnostic (local LLM + DeepSeek), Rust workspace.

Status: **M1 — squelette daemon + CLI + LLM local**. See `C:\Users\Ghota\.claude\plans\je-souhaite-cr-er-dynamic-moore.md` for the full roadmap.

## Quick start (M1)

```powershell
cp .env.example .env       # fill in real values
cp jarvis.toml.example jarvis.toml
cargo build --workspace
cargo run --bin jarvis-daemon -- run     # starts gRPC server
cargo run --bin jarvis -- ping           # in another shell
cargo run --bin jarvis -- ask "list 3 rust crates for actors"
```

## Crate layout

| Crate | Role |
|---|---|
| `jarvis-core` | Types, traits, errors. No I/O. |
| `jarvis-api` | gRPC `.proto` + generated client/server. |
| `jarvis-config` | TOML + env loader (figment). |
| `jarvis-llm` | `LlmProvider` trait + OpenAI-compatible impl. |
| `jarvis-daemon` | Long-running server. |
| `jarvis-cli` | Thin gRPC client (binary `jarvis`). |
