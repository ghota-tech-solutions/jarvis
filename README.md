# Jarvis

> An institutional-quality, continuously-running autonomous coding-agent system, written in Rust.

[![CI](https://github.com/ghota-tech-solutions/jarvis/actions/workflows/rust.yml/badge.svg)](https://github.com/ghota-tech-solutions/jarvis/actions/workflows/rust.yml)
[![Web](https://github.com/ghota-tech-solutions/jarvis/actions/workflows/web.yml/badge.svg)](https://github.com/ghota-tech-solutions/jarvis/actions/workflows/web.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)
[![Rust 1.95+](https://img.shields.io/badge/rust-1.95%2B-orange.svg)](https://www.rust-lang.org)

**Status: `v0.1.0-dev` — active development.** Core daemon, agent loop, sandboxes,
multi-model registry, SolidJS SPA, scheduling, GitHub PR integration, multi-model
verdict validator, and the replay UI have all shipped.

Jarvis is built to **work continuously** on the tasks you hand it, instead of
answering turn-by-turn like a chatbot. It goes beyond OpenCode, Claude Code,
Hermes, and Codex on four axes that nobody else tackles properly:

1. **Event-sourced ledger** (SQLite, append-only) as primary memory — not the
   LLM context. The agent queries its own past before acting, and its work
   survives restarts. The ledger feeds the **scrubbable canvas timeline** in
   the SPA.
2. **Multi-model registry** with probed capabilities and automatic quarantine:
   you add a model by editing `jarvis.toml`, zero code. The router picks by
   required capability, fails over on its own, and surfaces cost + tokens in
   real time in the bottom-screen HUD.
3. **Daemon + gRPC clients**: you can close the TUI or the SPA and the work
   keeps going. CLI, TUI, SolidJS SPA, Tauri desktop — and mobile soon — all
   share the same state over gRPC + gRPC-Web (`tonic-web`), authenticated by a
   loopback bearer token.
4. **Diff-by-intent + phase-gating**: instead of staging files, the agent
   groups its writes by parent decision (event-sourced via `parent_evt`). The
   user approves / rejects **an entire phase**; an atomic Git commit lands per
   phase. No other tool does this.

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│   Tauri 2 desktop (Win/Mac/Linux) — wraps the SPA bundle          │
│   + tray, jarvis:// deep links, native notifications, daemon       │
│     sidecar                                                        │
└──────────────────────────────────────────────────────────────────┘
                            ▲ (loads the same bundle)
┌──────────────────────────────────────────────────────────────────┐
│   SolidJS bundle (Bun + Vite)                                      │
│   routes: /, /fleet, /memory, /task/:id, /schedules, /settings     │
│   features: timeline canvas, fleet DAG, diff-by-intent,            │
│             memory promotion, cost/sandbox HUD, Cmd+K palette       │
└──────────────────────────────────────────────────────────────────┘
            ▲ gRPC-Web (Connect-ES)
┌──────────────────────────────────────────────────────────────────┐
│   crates/jarvis-web — axum                                         │
│   • bearer-token auth (`<data_dir>/web.token`, 256-bit CSPRNG)     │
│   • static SPA via rust-embed (in release)                         │
│   • port :7879 (CSP + static + future SSE events)                  │
└──────────────────────────────────────────────────────────────────┘
                            ▲ in-process
┌──────────────────────────────────────────────────────────────────┐
│   jarvis-daemon — gRPC + gRPC-Web on :7777                         │
│   • Auth interceptor: every RPC requires a Bearer token            │
│   • Refuses 0.0.0.0 unless daemon.bind_lan=true                    │
│   • Orchestrator: LlmPool, agent loop, ledger, sandbox, MCP        │
│   • 19 RPCs: tasks, timeline, fleet, diff-by-intent, memory        │
└──────────────────────────────────────────────────────────────────┘
                            ▲
┌──────────────────────────────────────────────────────────────────┐
│   Auto-authenticated tonic clients                                 │
│   jarvis-cli  ·  jarvis-tui  ·  jarvis-desktop sidecar             │
└──────────────────────────────────────────────────────────────────┘
```

### Crates

| Crate | Role | Status |
|---|---|---|
| `jarvis-core` | Types, traits, errors. No I/O. | Stable |
| `jarvis-api` | gRPC proto + generated client/server + `auth` (token discovery + interceptors). | Stable |
| `jarvis-config` | TOML + env loader (figment, `${VAR}` interpolation and `$$` escape). | Stable |
| `jarvis-ledger` | SQLite WAL, append-only event store, **memories table** (mutable), broadcast bus. | Stable |
| `jarvis-llm` | `LlmProvider` trait, OpenAI-compatible client, `ModelRegistry`, `LlmPool`, `pricing`. | Stable |
| `jarvis-tools` | `Tool` trait + 14 builtins: `fs_read`, `fs_write`, `shell`, `apply_patch`, `grep`, `glob`, `update_plan`, `web_search`, `spawn_subagent`, **`search_tools`** (meta-tool with lazy catalog support), `gh.pr_list`, `gh.pr_view`, `gh.pr_comment`, `gh.pr_create`. Args validated against each tool's JSON-Schema before invoke. | Stable |
| `jarvis-sandbox` | `NativeSandbox`, `DockerSandbox`, **`WslSandbox`** (Windows), `WorktreeManager`. | Stable |
| `jarvis-mcp` | MCP client + tool adapter — plugs in external MCP servers (stdio). | Stable |
| `jarvis-mcp-server` | **`jarvis-mcp-server` binary** — exposes Jarvis as an MCP server over stdio. 7 tools (`jarvis_ping`, `jarvis_ask`, `jarvis_submit_task`, …) wrap the daemon gRPC. Lets Claude Code / Codex / Cline drive Jarvis as a sub-routine. | Stable |
| `jarvis-agent` | Plan-act-observe loop, tolerant JSON parser, **loop detector**, **memory extractor**, system-prompt assembly with memories + **hierarchical AGENTS.md cascade**. Lifecycle hooks (pre/post/on_error). Six tool-call dialects: `json`, `gemma4_strict`, `gemma4_native`, **`hermes_xml`**, **`llama_python`**, **`tool_code_block`**. | Stable |
| `jarvis-repomap` | **Tree-sitter ranked repo map** — extracts symbols + reference graph, ranks by PageRank, fits the top-K into a token budget. Default-context-provider candidate. CLI: `repomap <path> [--budget N]`. | Stable (Rust extraction; Python/JS/TS parsers wired, queries pending) |
| `jarvis-bench` | **Reproducible task harness** — YAML suite + tempdir-per-task runner + scorecard JSON. Includes `benches/basic.yaml` mini-suite + stub provider fallback. The linchpin for measuring evolution impact. | Stable |
| `jarvis-daemon` | `jarvis-daemon` binary. gRPC + tonic-web auth, orchestrator, 19 RPCs. Ledger now uses **versioned migrations** under `crates/jarvis-ledger/migrations/`. | Stable |
| `jarvis-cli` | `jarvis` binary — scriptable gRPC client. Auto-loads token. | Stable |
| `jarvis-tui` | `jarvis-tui` binary — ratatui front-end, OpenCode-style. Auto-loads token. | Stable |
| `jarvis-web` | Axum crate: bearer-token auth, static SPA (rust-embed), SolidJS bundle in `ui/`. | Stable |
| `jarvis-desktop` | Tauri 2 wrapper — daemon sidecar, IPC (daemon_status, read_web_token), tray. | Scaffold; placeholder icons must be replaced before a signed ship. |

Strict DAG: `core` is the leaf; clients (`cli`, `tui`) depend only on `api` + `core`.
The SolidJS SPA generates its TypeScript client from the same proto via
`@bufbuild/protoc-gen-es` — one contract, shared front and back.

---

## Quick start

### Prerequisites

- Rust **1.95+** (edition 2024)
- [Bun](https://bun.sh) 1.3+ for the SPA
- `protoc` (vendored via `protoc-bin-vendored`, nothing to install)
- Optional: Docker (sandbox), a WSL2 distro (Windows sandbox)
- Tauri 2 desktop: `cargo install tauri-cli --version "^2.0"`

### Build and run

```powershell
# 1. Clone and build the Rust workspace
git clone https://github.com/ghota-tech-solutions/jarvis
cd jarvis
cargo build --release --workspace

# 2. First-time configuration
cp .env.example .env                 # fill HERMES_LOCAL_*, DEEPSEEK_API_KEY, etc.
cp jarvis.toml.example jarvis.toml   # tweak models, sandbox, routing

# 3. Start the daemon (shell 1)
.\target\release\jarvis-daemon.exe run
#  → gRPC + gRPC-Web : 127.0.0.1:7777 (bearer-token auth)
#  → SPA host        : 127.0.0.1:7879 (bearer-token auth)
#  → persistent token: .jarvis/web.token (0o600 on Unix)

# 4. Use it — your choice of client
.\target\release\jarvis-tui.exe       # ratatui TUI
.\target\release\jarvis.exe ping      # scriptable CLI
```

### Open the SPA

```powershell
# A static build will be embedded in the jarvis-web binary (upcoming).
# For now, run Vite in dev mode:
cd crates/jarvis-web/ui
bun install
bun run dev
# Open the URL Vite prints, then append the token:
#   http://127.0.0.1:5173/#token=<contents of .jarvis/web.token>
# The SPA strips the hash after bootstrap (token kept in sessionStorage).
```

### Tauri 2 desktop wrapper

```powershell
cd crates/jarvis-desktop
cargo tauri dev    # spawns vite + the daemon sidecar, opens the app
# or
cargo tauri build  # native build (Windows .msi/.exe, macOS .dmg,
                   #               Linux .AppImage/.deb/.rpm)
# Note: the shipped icons are 64x64 placeholders. Replace them before a
#       production ship via `cargo tauri icon path/to/icon.png`.
```

### Installation (distributed binaries)

Releases publish **unsigned bundles** for all three OSes via
`.github/workflows/release.yml`. Tagging `vX.Y.Z` triggers the workflow; the
bundles land in a **draft release** that the maintainer reviews, then publishes
manually.

| OS | Format | Warning | Bypass |
|---|---|---|---|
| Windows | `.msi` / `.nsis` (NSIS .exe) | SmartScreen "Unknown publisher" | "More info" → "Run anyway" |
| macOS | `.dmg` + `.app.tar.gz` | Gatekeeper "cannot be opened" | System Settings → Security → "Open Anyway"; or `xattr -dr com.apple.quarantine /Applications/Jarvis.app` |
| Linux | `.AppImage` / `.deb` / `.rpm` | none | `chmod +x Jarvis-*.AppImage && ./Jarvis-*.AppImage` |

> Code signing is deferred (Apple Developer ID + Windows EV cert). For now,
> builds are reproducible from a git tag → checksums available in the draft
> release artifacts.

### LAN / mobile access

The daemon refuses `0.0.0.0` by default. To expose it on the LAN:

```toml
# jarvis.toml
[daemon]
addr = "0.0.0.0:7777"
bind_lan = true       # required as soon as addr is non-loopback
```

Then, from your phone on the same Wi-Fi:

```
http://<host-lan-ip>:5173/#token=<cat .jarvis/web.token>
```

The bearer token gates the RPCs; binding `0.0.0.0` does not open a blind hole.

---

## Local LLM setup

Jarvis talks to any OpenAI-compatible endpoint — vLLM, llama.cpp, Ollama, and
LM Studio all work. The fastest path on Apple Silicon is
[**omlx**](https://github.com/jundot/omlx), an MLX-backed server with a built-in
admin UI for model downloads and tuning.

### Install omlx (macOS, Apple Silicon)

```bash
brew tap jundot/omlx
brew install --head --with-grammar omlx
omlx serve              # default port 8000
```

Open the admin UI and pull the two Gemma 4 variants Jarvis uses, via **Models →
[Downloader](http://localhost:8000/admin/dashboard?tab=models&modelsTab=downloader)**:

- `gemma-4-26B-A4B-it-MLX-4bit` — 4-bit quant, the model Jarvis runs against
  (`HERMES_LOCAL_MODEL`).
- `gemma-4-26B-A4B-it-assistant-bf16` — full precision, used **only** as the
  speculative-decoding drafter (see below).

### Tuning omlx for Jarvis

omlx persists tuning to two JSON files; editing them directly is more reliable
than the admin UI gear. Stop the server before editing, then restart it.

**`~/.omlx/model_settings.json`** — per-model settings, keyed by model name.
For `gemma-4-26B-A4B-it-MLX-4bit`, enable VLM MTP speculative decoding with the
bf16 assistant as the drafter:

```jsonc
{
  "gemma-4-26B-A4B-it-MLX-4bit": {
    // VLM MTP speculative decoding — ~80-90% token acceptance on code.
    "vlm_mtp_enabled": true,
    // Absolute path to the downloaded bf16 assistant model.
    "vlm_mtp_draft_model": "<omlx-models-dir>/gemma-4-26B-A4B-it-assistant-bf16",

    // TurboQuant KV is mutually exclusive with vlm_mtp — must be off.
    "turboquant_kv_enabled": false,

    // DFlash crashes on the gemma4 assistant drafter — must be off. Also
    // drop every dflash_* key (draft_model, quant_*, in_memory_cache_*,
    // ssd_cache, verify_mode) and specprefill_draft_model entirely.
    "dflash_enabled": false
  }
}
```

**`~/.omlx/settings.json`** — global server settings:

```jsonc
{
  "scheduler": {
    // Better time-to-first-token on long agent prompts.
    "chunked_prefill": true
  },
  "cache": {
    // Prefix cache held in RAM — visible win on prompts ≥1024 tokens.
    "hot_cache_max_size": "8GB"
  }
}
```

With this config: no DFlash warning, VLM MTP active with the bf16 drafter,
~80-90% MTP acceptance on code (vs ~66% on short text), an 8 GB RAM hot cache,
and ~27.6 → 30.4 tok/s on short prompts — a larger gain on the long prompts the
agent actually sends.

Smoke-test the chat at [/admin/chat](http://localhost:8000/admin/chat) before
wiring Jarvis.

> **Linux / Windows**: omlx is macOS-only. On Linux use
> [vLLM](https://github.com/vllm-project/vllm) or
> [llama.cpp](https://github.com/ggerganov/llama.cpp); on Windows use WSL2 with
> either, or [LM Studio](https://lmstudio.ai/). Any OpenAI-compatible server
> works — only `HERMES_LOCAL_URL` and `HERMES_LOCAL_MODEL` change.

### `.env`

`cp .env.example .env`, then point Jarvis at the endpoint:

```bash
# Local LLM (OpenAI-compatible endpoint — omlx, MLX-LM, llama.cpp, vLLM)
HERMES_LOCAL_URL=http://127.0.0.1:8000/v1
HERMES_LOCAL_MODEL=gemma-4-26B-A4B-it-MLX-4bit
HERMES_LOCAL_API_KEY=azer            # any non-empty string — omlx ignores it,
                                     # the OpenAI SDK just requires the field

# Remote LLM (DeepSeek API — optional, only for failover)
#DEEPSEEK_API_KEY=sk-xxx

# Daemon — bind 0.0.0.0 so LAN devices (e.g. mobile testing) can reach the
# gRPC + gRPC-Web endpoint. The bearer token still gates access.
JARVIS_DAEMON_ADDR=0.0.0.0:7777
# JARVIS_SPA_ADDR overrides jarvis-web's bind. Default is 0.0.0.0:7879.
JARVIS_LOG=jarvis=debug,info

# Web search — Brave Search API (free tier:
# https://api.search.brave.com/app/dashboard). The var name avoids the
# JARVIS_ prefix, reserved for jarvis.toml field overrides (figment strict).
#WEB_SEARCH_BACKEND=brave
#BRAVE_API_KEY=xxx
#TAVILY_API_KEY=tvly-xxxxxxxxxxxxxxxxxxxxxxxx
```

---

## Configuration (`jarvis.toml`)

```toml
[daemon]
addr = "127.0.0.1:7777"
data_dir = ".jarvis"
# Opt-in for non-loopback binds. The daemon refuses to start otherwise.
bind_lan = false
# Disables auth (DANGER — local, fully-trusted dev only).
disable_auth = false

# --- Local models (multi-entry) ---
[providers.local.gemma]
url      = "${HERMES_LOCAL_URL}"
model    = "${HERMES_LOCAL_MODEL}"
api_key  = "${HERMES_LOCAL_API_KEY}"
priority = 10
capabilities = { ctx_len = 128000, tool_calls = true, json_schema = true, vision = false }

# --- Remote models ---
[providers.remote.deepseek_pro]
url      = "https://api.deepseek.com/v1"
model    = "deepseek-chat"
api_key  = "${DEEPSEEK_API_KEY:-disabled}"
priority = 7
cost_per_mtok_in  = 0.27       # $/M input tokens — feeds the HUD
cost_per_mtok_out = 1.10       # $/M output tokens
capabilities = { ctx_len = 200000, tool_calls = true, json_schema = true, vision = false }

# --- Routing ---
[routing]
default_policy             = "auto"        # auto | local_only | remote_only | model:<name>
monthly_remote_usd_cap     = 50
quarantine_after_failures  = 3
quarantine_window_minutes  = 10
quarantine_duration_minutes = 15

# --- Sandbox ---
[sandbox]
default_backend    = "native"              # native | docker | wsl2
default_net_policy = "egress_only"         # none | egress_only | full  (docker)
default_mode       = "workspace_write"     # read_only | workspace_write | danger_full_access
docker_image       = "alpine:3.20"
docker_memory      = "2g"
docker_cpus        = 2.0
docker_autopull    = true

# --- MCP servers ---
[mcp.servers.mock]
enable  = true
command = "jarvis-mcp-mock"
args    = []

# --- Post-tool hooks (auto `cargo check`, etc.) ---
[[hooks.post_tool]]
match    = "apply_patch|fs_write"
cmd      = "cargo check --workspace"
label    = "cargo check"
timeout_s = 60
```

Environment variables that complement `jarvis.toml`:

| Var | Effect |
|---|---|
| `JARVIS_DAEMON_ADDR` | Overrides `daemon.addr` (env > toml > defaults). |
| `JARVIS_SPA_ADDR` | Overrides the SPA host address (default `0.0.0.0:7879`). |
| `JARVIS_DATA_DIR` | Overrides `daemon.data_dir`. Also where clients look for `web.token`. |
| `JARVIS_WEB_TOKEN` | Directly overrides the client-side bearer token (CI, scripts). |
| `JARVIS_SEARCH_BACKEND` | `brave` or `tavily` for the `web_search` tool. |
| `BRAVE_API_KEY` / `TAVILY_API_KEY` | API keys for `web_search`. |

---

## CLI — `jarvis`

```
# Health + status
jarvis ping
jarvis status                                  # lists models + state

# Raw LLM (one-shot, bypasses the agent)
jarvis ask "List 3 Rust crates for actors"

# Autonomous tasks
jarvis task add "Refactor src/ to use anyhow"  # workdir = cwd by default
jarvis task add --workdir C:\proj\app --watch "Add a POST /users route"
jarvis task add --sandbox docker --net egress_only --worktree --watch \
   "Run cargo test and fix any failures"
jarvis task add --sandbox wsl2 --watch \
   "Run a Linux-only build in /mnt/c/proj"
jarvis task add --model local:fallback "..."   # force a specific model
jarvis task add --routing remote_only "..."    # remote only
jarvis task add --require vision "Describe this image"
jarvis task add --resume <task-uuid> "..."     # resume after a crash

# Management
jarvis task list           # active tasks only
jarvis task list --all     # including finished
jarvis task get <id>
jarvis task watch <id>     # live event stream
jarvis task cancel <id>
```

### `task add` flags

| Flag | Effect |
|---|---|
| `--workdir <path>` | Agent working directory (default: cwd). |
| `--max-steps N` | Cap on the agent loop (default 20). |
| `--sandbox native\|docker\|wsl2` | Execution backend. Default = config. |
| `--net none\|egress_only\|full` | Network policy (Docker only). |
| `--worktree` | Creates an isolated git worktree if workdir is a repo. |
| `--base-ref <ref>` | Base Git ref for the worktree (default `HEAD`). |
| `--routing <policy>` | `auto`, `local_only`, `remote_only`, or `model:<name>`. |
| `--model <name>` | Shorthand for `--routing model:<name>`. |
| `--require <cap>` | Required capability (repeatable): `tool_calls`, `json_schema`, `vision`. |
| `--parent <uuid>` | Continues a conversation: parent task, inherits workdir/sandbox/net. |
| `--resume <uuid>` | Resumes an interrupted task: inherits everything, logs a `Continuation` event. |
| `--watch` | Streams live events after submission. |

---

## SPA (`crates/jarvis-web/ui`) — the primary front-end

Six routes in a single SolidJS page, shared by Web and Tauri.

| Route | Key component |
|---|---|
| `/` | Quick Ask (streamed one-shot chat) + Tasks grid grouped by parent + New-task form + goal/id/workdir filter + status selector |
| `/task/:id` | Header + view toggle (both \| timeline \| transcript \| diff) + markdown transcript + **scrubbable canvas timeline** (4 lanes) with **replay mode 1×/2×/4×/8×** + DiffByIntent + Cancel/Continue |
| `/fleet` | Hand-rolled SVG DAG, parent → child oriented, auto-bubbling of "attention" tasks |
| `/memory` | Candidates / Active, edit-in-place, promote/forget/dismiss, usage counters |
| `/schedules` | Cron-driven autonomous runs. CRUD + run-now + paused state. |
| (footer) | Permanent HUD: active model · running · real tokens in/out · $ vs cap · sandbox badge · live indicator |

**Cmd/Ctrl+K** opens the command palette (fuzzy match over 8 commands:
navigation, theme, new task, spawn-explorer, quick ask…). `?` opens the
shortcuts overlay (drag the playhead, `j`/`k` navigate events, `[`/`]` jump
spans, `1`-`4` toggle lanes, etc.).

The bundle is built with `bun run build`; it will be embedded into `jarvis-web`
via `rust-embed`.

---

## TUI — `jarvis-tui`

A borderless interface inspired by OpenCode. **Stable but in maintenance mode**
since the SPA took product priority. Covers "terminal-only" use and machines
without a GUI.

```
┌──────────────────────────────────────────────┬────────────────────────┐
│ Implement JWT auth                            │ ▼ Task                 │
│ running · docker/egress_only · 7f3a2b1c       │ id      7f3a2b1c       │
│                                                │ status  running        │
│ Analyzing the existing code…                  │                        │
│ → fs_read src/auth.rs                          │ ▼ Sandbox              │
│   ✓ read 1248 bytes                            │ backend  docker        │
│ → shell cargo build --tests                    │ network  egress_only   │
│   ✓ [docker] cargo build → exit=0              │                        │
│ ─ step 2 ─                                     │ ▼ Models               │
│ Adding the JWT middleware…                     │ ● gemma · local        │
├──────────────────────────────────────────────┴────────────────────────┤
│ ❯ type to send  ·  :cancel  :all  :refresh  :theme  :q                │
└────────────────────────────────────────────────────────────────────────┘
```

Keys: `j/k`, `↓/↑`, `c` cancel, `r` refresh, `a` toggle active ↔ all, `:` slash
palette, `?` help, `Esc` quit (on an empty input).

You can **kill the TUI** at any time and the daemon keeps working. Relaunch
`jarvis-tui` to rejoin the state (gRPC re-sync + event replay).

---

## The ledger

Every agent action is written append-only into `.jarvis/ledger.sqlite` (WAL,
anti-`UPDATE`/`DELETE` triggers). You can explore it:

```sql
sqlite3 .jarvis/ledger.sqlite

> SELECT id, kind, subject, json_extract(payload, '$.tool') AS tool
  FROM events WHERE task_id = ? ORDER BY id;
```

Event kinds: `attempt`, `decision`, `tool_call`, `tool_result`, `observation`,
`error`, `verdict`, `spawn`, `heartbeat`, `continuation`, `llm_chunk`.

The `memories` table is **mutable**: promote/forget/edit via the SPA `/memory`
route.

---

## Tests + CI

```powershell
cargo test --workspace --lib              # unit test suite
cargo clippy --workspace -- -D warnings   # zero warnings
cargo fmt --all -- --check                # zero diff
cd crates/jarvis-web/ui && bun run typecheck && bun run build
```

**CI** (`.github/workflows/`):
- `rust.yml` — fmt + clippy + test on an **ubuntu + windows** matrix
- `web.yml` — Bun install + typecheck + vite build + 2 MB budget on `dist/`
- `proto.yml` — `buf lint` (the module is defined in `crates/jarvis-api/proto/buf.yaml`)
- `release.yml` — cross-OS release matrix, triggered by a `vX.Y.Z` tag

See [CONTRIBUTING.md](./CONTRIBUTING.md) for the full development workflow.

---

## Roadmap

- [x] **M1** — daemon + CLI + local LLM skeleton
- [x] **M2** — event-sourced ledger + agent loop
- [x] **M3** — Native + Docker sandboxes + worktrees + parallelism
- [x] **M4** — multi-pane ratatui TUI
- [x] **M5** — multi-model registry + capability-aware routing + quarantine
- [x] **M6** — SPA rebuild (SolidJS + Vite + Bun) + **scrubbable canvas timeline** (flagship)
- [x] **M7** — Fleet DAG + cost/sandbox HUD + MCP client
- [x] **M8** — **Diff-by-intent + phase-gating** (atomic Git commits per parent decision)
- [x] **M9** — **Memory promotion** (post-verdict LLM extractor + system-prompt injection + curation UI)
- [x] **M10** — gRPC bearer-token auth + CI workflows + WSL2 sandbox + loop detector + resume_from
- [x] **M11** (partial) — Cmd+K palette · `web_search` · `spawn_subagent` (explorer/worker/reviewer) · per-turn cost streaming · multi-model verdict validator
- [x] **M12** — Cron/scheduling (`/schedules`) · GitHub PR via `gh` · audit replay UI (▶/2×/4×/8×) · auto reviewer sub-agent · GitHub Actions release matrix
- [x] **M13** — Tier 1 strategic evolutions (see `~/.claude/plans/analyse-le-projet-en-snappy-marble.md` for full plan):
  - **Hierarchical AGENTS.md cascade** — repo → subdirs → workdir, accumulated under an 8 KB budget
  - **Lifecycle hooks** — `pre/post/on_error` phases with `hook:<phase>:<label>` synth events
  - **Tool-call dialects** — `hermes_xml`, `llama_python`, `tool_code_block` (any open-weights model becomes pluggable)
  - **`search_tools` meta-tool** + opt-in compact catalog rendering ([agent].lazy_tool_catalog) — saves 3-5 k tokens/turn
  - **JSON-Schema args enforcement** — invalid tool args rejected before invoke, with clear errors the model can self-correct against
  - **Versioned ledger migrations** — `crates/jarvis-ledger/migrations/` + new indexes (parent_evt, memories, tasks.parent)
  - **`jarvis-repomap`** — tree-sitter ranked symbol map, the default-context-provider candidate
  - **`jarvis-bench`** — YAML-suite + scorecard JSON harness, the linchpin for measuring evolution impact
  - **`jarvis-mcp-server`** — Jarvis becomes an MCP server (7 tools wrapping the daemon RPCs); Claude Code/Codex/Cline can drive Jarvis as a sub-routine
- [ ] **M11** (remaining) — image read + generation · browser sidecar (chromiumoxide)

---

## Design decisions

| Question | Answer | Why |
|---|---|---|
| Language | **Rust** | Performance, type safety, mature async ecosystem. |
| Process architecture | **Daemon + clients** | The TUI/SPA can be killed; the work continues. Multi-client over gRPC. |
| Agent memory | **Append-only ledger** (SQLite) | The LLM context is disposable; attempts/decisions are permanent and queryable. Feeds the scrubbable timeline. |
| Orchestration | **Supervisor/worker** (Tokio tasks) | A pattern that has shipped in production. No swarm. |
| Isolation | **3 sandboxes: Native + Docker + WSL2** | Native = fast debugging. Docker = kernel-level isolation. WSL2 = "Linux on Windows without Docker". |
| Network policy | **3 modes: none / egress_only / full** | `egress_only` is the sensible default (allows `cargo build`, `npm install`). |
| Multi-model | **Registry** (N local + N remote), capability-aware routing | No lock-in. Adding a model = editing TOML, zero code. Automatic failover via quarantine. |
| Front-end | **SolidJS + Vite + Bun + Tauri 2** | Fine-grained reactivity for gRPC streams; tiny bundle; Tauri wraps the SAME bundle. Not two codebases. |
| Browser ↔ gRPC bridge | **`tonic-web` + Connect-ES** | One proto contract, TS client generated by `bufbuild/protoc-gen-es`. |
| Auth | **Loopback bearer token** (256-bit, `.jarvis/web.token`) | Auto-discovery client-side. No mTLS until multi-user. |
| TUI borders | **None** (borderless, OpenCode-inspired) | Readability, modern look. Whitespace > box-drawing. |

---

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](./CONTRIBUTING.md) for the dev
environment, commit conventions, and review expectations, and
[CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md) for community standards. Security
issues: please follow the process in [SECURITY.md](./SECURITY.md).

## License

Released under the [MIT License](./LICENSE).
