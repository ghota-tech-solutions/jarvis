# Jarvis

> Système d'agents de codage autonome et continu, à la qualité institutionnelle, en Rust.

**Statut : `v0.1.0-dev` — M12 majoritairement livré · daemon + agent + sandbox + multi-modèles + SPA SolidJS + scheduling + GitHub PR + verdict validator + replay UI · 105 tests verts**

Jarvis est conçu pour **travailler en continu** sur les tâches que tu lui donnes, plutôt
que de répondre tour par tour comme un chatbot. Il dépasse OpenCode, Claude Code,
Hermes et Codex sur quatre axes que personne d'autre n'attaque correctement :

1. **Ledger event-sourced** (SQLite, append-only) comme mémoire primaire — pas le
   contexte LLM. L'agent interroge son passé avant d'agir, et son travail survit
   aux redémarrages. Le ledger nourrit la **timeline canvas scrubable** dans la SPA.
2. **Registry multi-modèles** avec capabilities probées + quarantaine automatique :
   tu ajoutes un modèle en éditant `jarvis.toml`, zéro code. Le routeur choisit par
   capability requise, bascule tout seul, et exhibe coût + tokens en temps réel
   dans le HUD bas-écran.
3. **Daemon + clients gRPC** : tu peux fermer la TUI ou la SPA, le travail continue.
   CLI, TUI, SPA SolidJS, Tauri desktop, et bientôt mobile — tous partagent le
   même état via gRPC + gRPC-Web (`tonic-web`), authentifié par un bearer token
   loopback.
4. **Diff-by-intent + phase-gating** : au lieu de stager les fichiers, l'agent
   regroupe ses écritures par décision parent (event-sourced via `parent_evt`).
   L'utilisateur approuve / rejette **une phase entière** ; un commit Git
   atomique tombe par phase. Aucune autre tool n'a ça.

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│   Tauri 2 desktop (Win/Mac/Linux) — wrappe le bundle SPA         │
│   + tray, deep links jarvis://, notifs natives, sidecar daemon   │
└──────────────────────────────────────────────────────────────────┘
                            ▲ (charge le même bundle)
┌──────────────────────────────────────────────────────────────────┐
│   Bundle SolidJS (Bun + Vite)                                    │
│   routes: /, /fleet, /memory, /task/:id, /settings (todo)        │
│   features: timeline canvas, fleet DAG, diff-by-intent,          │
│             memory promotion, HUD coût/sandbox, Cmd+K palette    │
└──────────────────────────────────────────────────────────────────┘
            ▲ gRPC-Web (Connect-ES)
┌──────────────────────────────────────────────────────────────────┐
│   crates/jarvis-web — axum                                       │
│   • bearer-token auth (`<data_dir>/web.token`, 256 bits CSPRNG)  │
│   • static SPA via rust-embed (en release)                       │
│   • port :7879 (CSP + static + future SSE événements)            │
└──────────────────────────────────────────────────────────────────┘
                            ▲ in-process
┌──────────────────────────────────────────────────────────────────┐
│   jarvis-daemon — gRPC + gRPC-Web sur :7777                      │
│   • Interceptor d'auth: tous RPCs exigent Bearer token           │
│   • Refuse 0.0.0.0 sans daemon.bind_lan=true                     │
│   • Orchestrator: LlmPool, agent loop, ledger, sandbox, MCP      │
│   • 19 RPCs: tasks, timeline, fleet, diff-by-intent, memory      │
└──────────────────────────────────────────────────────────────────┘
                            ▲
┌──────────────────────────────────────────────────────────────────┐
│   Clients tonic auto-authentifiés                                │
│   jarvis-cli  · jarvis-tui  · jarvis-desktop sidecar             │
└──────────────────────────────────────────────────────────────────┘
```

### Crates

| Crate | Rôle | Status |
|---|---|---|
| `jarvis-core` | Types, traits, erreurs. Pas d'I/O. | Stable |
| `jarvis-api` | Proto gRPC + client/server généré + `auth` (token discovery + interceptors). | Stable |
| `jarvis-config` | Loader TOML + env (figment, interpolation `${VAR}` et `$$` escape). | Stable |
| `jarvis-ledger` | SQLite WAL, event store append-only, **memories table** (mutable), broadcast bus. | Stable |
| `jarvis-llm` | `LlmProvider` trait, OpenAI-compat client, `ModelRegistry`, `LlmPool`, `pricing`. | Stable |
| `jarvis-tools` | `Tool` trait + 12 builtins : `fs_read`, `fs_write`, `shell`, `apply_patch`, `grep`, `glob`, `update_plan`, `web_search`, `spawn_subagent`, `gh.pr_list`, `gh.pr_view`, `gh.pr_comment`, `gh.pr_create`. | Stable |
| `jarvis-sandbox` | `NativeSandbox`, `DockerSandbox`, **`WslSandbox`** (Windows), `WorktreeManager`. | Stable |
| `jarvis-mcp` | MCP client + tool adapter — branche des serveurs MCP externes (stdio). | Stable |
| `jarvis-agent` | Boucle plan-act-observe, parser JSON tolérant, **loop detector**, **memory extractor**, system-prompt assembly avec memories + AGENTS.md. | Stable |
| `jarvis-daemon` | Binaire `jarvis-daemon`. Auth gRPC + tonic-web, orchestrateur, 16 RPCs implémentées. | Stable |
| `jarvis-cli` | Binaire `jarvis` — client gRPC scriptable. Auto-load token. | Stable |
| `jarvis-tui` | Binaire `jarvis-tui` — front-end ratatui, look OpenCode. Auto-load token. | Stable |
| `jarvis-web` | Crate Axum : bearer-token auth, SPA static (rust-embed), SolidJS bundle dans `ui/`. | Stable |
| `jarvis-desktop` | Wrapper Tauri 2 — sidecar daemon, IPC (daemon_status, read_web_token), tray. | Scaffold ; icônes placeholder à remplacer pour ship signé. |

DAG strict : `core` est leaf ; clients (`cli`, `tui`) ne dépendent que de `api` + `core`.
La SPA SolidJS génère son client TypeScript depuis le même proto via `@bufbuild/protoc-gen-es` — un seul contrat, partagé front + back.

---

## Quick start

### Prérequis

- Rust **1.95+** (edition 2024)
- [Bun](https://bun.sh) 1.3+ pour la SPA
- `protoc` (vendored via `protoc-bin-vendored`, rien à installer)
- Optionnel : Docker (sandbox), WSL2 distro (sandbox Windows)
- Tauri 2 desktop : `cargo install tauri-cli --version "^2.0"`

### Build et démarrage

```powershell
# 1. Cloner et builder le workspace Rust
cargo build --release --workspace

# 2. Configurer (la première fois)
cp .env.example .env                 # remplir HERMES_LOCAL_*, DEEPSEEK_API_KEY, etc.
cp jarvis.toml.example jarvis.toml   # ajuster modèles, sandbox, routing

# 3. Démarrer le daemon (shell 1)
.\target\release\jarvis-daemon.exe run
#  → gRPC + gRPC-Web : 127.0.0.1:7777 (auth bearer token)
#  → SPA host        : 127.0.0.1:7879 (auth bearer token)
#  → token persistant : .jarvis/web.token (0o600 sur Unix)

# 4. Utiliser — au choix
.\target\release\jarvis-tui.exe       # TUI ratatui
.\target\release\jarvis.exe ping      # CLI scriptable
```

### Ouvrir la SPA

```powershell
# Build statique embeddé dans le binaire jarvis-web (futur)
# ou — pour l'instant — Vite en dev :
cd crates/jarvis-web/ui
bun install
bun run dev
# Ouvrir l'URL affichée par Vite, puis ajouter le token :
#   http://127.0.0.1:5173/#token=<contenu de .jarvis/web.token>
# La SPA strip le hash après bootstrap (token en sessionStorage).
```

### Wrapper desktop Tauri 2

```powershell
cd crates/jarvis-desktop
cargo tauri dev    # spawn vite + sidecar daemon, ouvre l'app
# ou
cargo tauri build  # build natif (Windows .msi, Mac .dmg, Linux .AppImage)
# Note: les icônes shipped sont placeholders 64x64. Remplacer avant ship
#       prod via `cargo tauri icon path/to/icon.png`.
```

### Accès LAN / mobile

Le daemon refuse `0.0.0.0` par défaut. Pour exposer sur le LAN :

```toml
# jarvis.toml
[daemon]
addr = "0.0.0.0:7777"
bind_lan = true       # obligatoire dès que addr est non-loopback
```

Puis depuis ton téléphone sur le même Wi-Fi :

```
http://<host-lan-ip>:5173/#token=<cat .jarvis/web.token>
```

Le bearer token gate les RPCs ; le bind 0.0.0.0 n'ouvre pas un trou aveugle.

---

## Configuration (`jarvis.toml`)

```toml
[daemon]
addr = "127.0.0.1:7777"
data_dir = ".jarvis"
# Opt-in pour bind non-loopback. Le daemon refuse de démarrer sinon.
bind_lan = false
# Désactive l'auth (DANGER — uniquement local dev fully trusted).
disable_auth = false

# --- Modèles locaux (multi-entrées) ---
[providers.local.gemma]
url      = "${HERMES_LOCAL_URL}"
model    = "${HERMES_LOCAL_MODEL}"
api_key  = "${HERMES_LOCAL_API_KEY}"
priority = 10
capabilities = { ctx_len = 128000, tool_calls = true, json_schema = true, vision = false }

# --- Modèles remote ---
[providers.remote.deepseek_pro]
url      = "https://api.deepseek.com/v1"
model    = "deepseek-chat"
api_key  = "${DEEPSEEK_API_KEY:-disabled}"
priority = 7
cost_per_mtok_in  = 0.27       # $/M input tokens — alimente le HUD
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

# --- MCP servers (M7) ---
[mcp.servers.mock]
enable  = true
command = "jarvis-mcp-mock"
args    = []

# --- Hooks post-tool (auto-`cargo check` etc.) ---
[[hooks.post_tool]]
match    = "apply_patch|fs_write"
cmd      = "cargo check --workspace"
label    = "cargo check"
timeout_s = 60
```

Variables d'environnement qui complètent jarvis.toml :

| Var | Effet |
|---|---|
| `JARVIS_DAEMON_ADDR` | Override de `daemon.addr` (env > toml > defaults). |
| `JARVIS_SPA_ADDR` | Override de l'addr du host SPA (default `0.0.0.0:7879`). |
| `JARVIS_DATA_DIR` | Override de `daemon.data_dir`. Aussi cherché pour `web.token` côté clients. |
| `JARVIS_WEB_TOKEN` | Override direct du bearer token côté client (CI, scripts). |
| `JARVIS_SEARCH_BACKEND` | `brave` ou `tavily` pour le tool `web_search` (M11.S2). |
| `BRAVE_API_KEY` / `TAVILY_API_KEY` | Clés API pour `web_search`. |

---

## CLI — `jarvis`

```
# Santé + statut
jarvis ping
jarvis status                                  # liste modèles + état

# LLM brut (one-shot, bypass agent)
jarvis ask "Liste 3 crates Rust pour les acteurs"

# Tâches autonomes
jarvis task add "Refactor src/ to use anyhow"  # workdir = cwd par défaut
jarvis task add --workdir C:\proj\app --watch "Ajoute une route POST /users"
jarvis task add --sandbox docker --net egress_only --worktree --watch \
   "Run cargo test and fix any failures"
jarvis task add --sandbox wsl2 --watch \
   "Run a Linux-only build in /mnt/c/proj"   # M10.S5
jarvis task add --model local:fallback "..."   # force un modèle précis
jarvis task add --routing remote_only "..."    # remote uniquement
jarvis task add --require vision "Décris cette image"
jarvis task add --resume <task-uuid> "..."     # M10.S4 : reprendre après crash

# Gestion
jarvis task list           # tâches actives seulement
jarvis task list --all     # incluant terminées
jarvis task get <id>
jarvis task watch <id>     # stream live des events
jarvis task cancel <id>
```

### Flags `task add`

| Flag | Effet |
|---|---|
| `--workdir <path>` | Dossier de travail de l'agent (défaut : cwd). |
| `--max-steps N` | Cap sur la boucle agent (défaut 20). |
| `--sandbox native\|docker\|wsl2` | Backend d'exécution. Défaut = config. |
| `--net none\|egress_only\|full` | Politique réseau (Docker uniquement). |
| `--worktree` | Crée un git worktree isolé si workdir est un repo. |
| `--base-ref <ref>` | Ref Git de base pour le worktree (défaut `HEAD`). |
| `--routing <policy>` | `auto`, `local_only`, `remote_only`, ou `model:<name>`. |
| `--model <name>` | Raccourci pour `--routing model:<name>`. |
| `--require <cap>` | Capacité requise (répétable) : `tool_calls`, `json_schema`, `vision`. |
| `--parent <uuid>` | Continue une conversation : task parent, hérite workdir/sandbox/net. |
| `--resume <uuid>` | Reprend une tâche interrompue : hérite tout, logue un event `Continuation`. |
| `--watch` | Streame les events live après soumission. |

---

## SPA (`crates/jarvis-web/ui`) — la vraie front-end

5 routes dans une seule page SolidJS, partagée par Web et Tauri.

| Route | Composant clef |
|---|---|
| `/` | Quick Ask (chat one-shot streamé) + Tasks grid groupé par parent + form New task + filter goal/id/workdir + status selector |
| `/task/:id` | Header + view toggle (both \| timeline \| transcript \| diff) + transcript markdown + **scrubbable canvas timeline** (4 lanes) avec **mode replay 1×/2×/4×/8×** + DiffByIntent + Cancel/Continue |
| `/fleet` | DAG SVG hand-rolled, parent → child orienté, auto-bubble des tâches "attention" |
| `/memory` | Candidates / Active, edit-in-place, promote/forget/dismiss, usage counters |
| `/schedules` | Cron-driven autonomous runs. CRUD + run-now + paused state. |
| (footer) | HUD permanent : modèle actif · running · tokens in/out réels · $ vs cap · badge sandbox · live indicator |

**Cmd/Ctrl+K** ouvre le command palette (fuzzy match sur 8 commandes : navigation, theme, new task, spawn-explorer, quick ask…). `?` ouvre l'overlay des shortcuts (drag playhead, jk navigate events, `[`/`]` jumps spans, 1-4 toggle lanes, etc.).

Le bundle se construit avec `bun run build` ; il sera embeddé dans `jarvis-web` via `rust-embed` au moment du M12.

---

## TUI — `jarvis-tui`

Interface borderless inspirée d'OpenCode. **Stable mais en mode maintenance** depuis que la SPA a la priorité produit. Couvre les usages "terminal-only" et machines sans GUI.

```
┌──────────────────────────────────────────────┬────────────────────────┐
│ Implémenter l'auth JWT                       │ ▼ Task                 │
│ running · docker/egress_only · 7f3a2b1c      │ id      7f3a2b1c       │
│                                              │ status  running        │
│ J'analyse le code existant…                  │                        │
│ → fs_read src/auth.rs                        │ ▼ Sandbox              │
│   ✓ read 1248 bytes                          │ backend  docker        │
│ → shell cargo build --tests                  │ network  egress_only   │
│   ✓ [docker] cargo build → exit=0            │                        │
│ ─ step 2 ─                                   │ ▼ Models               │
│ Je vais ajouter le middleware JWT…           │ ● gemma · local        │
├──────────────────────────────────────────────┴────────────────────────┤
│ ❯ type to send  ·  :cancel  :all  :refresh  :theme  :q                │
└────────────────────────────────────────────────────────────────────────┘
```

Touches : `j/k`, `↓/↑`, `c` cancel, `r` refresh, `a` toggle actives ↔ toutes, `:` slash palette, `?` help, `Esc` quit (sur input vide).

Tu peux **tuer la TUI** à tout moment, le daemon continue le travail. Relance `jarvis-tui` pour rejoindre l'état (gRPC re-sync + replay des events).

---

## Le ledger

Toutes les actions de l'agent sont écrites en append-only dans
`.jarvis/ledger.sqlite` (WAL, triggers anti-`UPDATE`/`DELETE`). Tu peux l'explorer :

```sql
sqlite3 .jarvis/ledger.sqlite

> SELECT id, kind, subject, json_extract(payload, '$.tool') AS tool
  FROM events WHERE task_id = ? ORDER BY id;
```

Types d'events : `attempt`, `decision`, `tool_call`, `tool_result`, `observation`,
`error`, `verdict`, `spawn`, `heartbeat`, `continuation`, `llm_chunk`.

La table `memories` (M9) est **mutable** : promote/forget/edit via la SPA `/memory`.

---

## Tests + CI

```powershell
cargo test --workspace --lib              # 91 tests, ~10s
cargo clippy --workspace -- -D warnings   # zéro warning
cargo fmt --all -- --check                # zéro diff
cd crates/jarvis-web/ui && bun run typecheck && bun run build
```

**CI** (`.github/workflows/`) :
- `rust.yml` — fmt + clippy + test sur matrice **ubuntu + windows**
- `web.yml` — Bun install + typecheck + vite build + budget 2 MB sur `dist/`
- `proto.yml` — `buf lint` (le module est défini dans `crates/jarvis-api/proto/buf.yaml`)

---

## Roadmap

- [x] **M1** — squelette daemon + CLI + LLM local
- [x] **M2** — ledger event-sourcing + boucle agent
- [x] **M3** — sandbox Native + Docker + worktrees + parallélisme
- [x] **M4** — TUI ratatui multi-pane
- [x] **M5** — registry multi-modèles + routage capability-aware + quarantaine
- [x] **M6** — refonte SPA (SolidJS + Vite + Bun) + **scrubbable canvas timeline** (flagship)
- [x] **M7** — Fleet DAG + HUD coût/sandbox + MCP client
- [x] **M8** — **Diff-by-intent + phase-gating** (commits Git atomiques par décision parent)
- [x] **M9** — **Memory promotion** (extractor LLM post-verdict + injection system prompt + UI curation)
- [x] **M10** — Auth gRPC bearer-token + CI workflows + WSL2 sandbox + loop detector + resume_from
- [x] **M11.S2/S3/S5/S6/S7** — Cmd+K palette · web_search · spawn_subagent (explorer/worker/reviewer) · cost per-turn streaming · multi-model verdict validator
- [x] **M12.S1/S2/S3/S4** — Cron/scheduling (`/schedules`) · GitHub PR via `gh` · audit replay UI (▶/2×/4×/8×) · auto reviewer sub-agent
- [ ] **M11.rest** — image read+gen · browser sidecar (chromiumoxide)
- [ ] **M12.S5** — Tauri release workflow (unsigned + AppImage)

Plan complet, décisions, risques : `~/.claude/plans/analyse-le-projet-l-ui-drifting-valley.md`.

---

## Décisions structurantes

| Question | Réponse | Pourquoi |
|---|---|---|
| Langage | **Rust** | Performance, type safety, écosystème async mature. |
| Architecture process | **Daemon + clients** | TUI/SPA peuvent être tués, le travail continue. Multi-clients via gRPC. |
| Mémoire de l'agent | **Ledger append-only** (SQLite) | Le contexte LLM est jetable ; attempts/decisions sont permanentes et interrogeables. Alimente la timeline scrubable. |
| Orchestration | **Supervisor/worker** (Tokio tasks) | Pattern qui a shippé en prod. Pas de swarm. |
| Isolation | **3 sandboxes : Native + Docker + WSL2** | Native = fast debug. Docker = isolation kernel-level. WSL2 = "Linux sur Windows sans Docker" (M10.S5). |
| Politique réseau | **3 modes : none / egress_only / full** | `egress_only` est le défaut sensé (permet `cargo build`, `npm install`). |
| Multi-modèles | **Registry** (N local + N remote), routing capability-aware | Pas de lock-in. Ajout = éditer TOML, zéro code. Failover auto via quarantaine. |
| Front | **SolidJS + Vite + Bun + Tauri 2** | Réactivité fine-grain pour streams gRPC ; bundle minuscule ; Tauri wrappe le MÊME bundle. Pas deux codebases. |
| Bridge browser↔gRPC | **`tonic-web` + Connect-ES** | Un seul contrat proto, client TS généré par `bufbuild/protoc-gen-es`. |
| Auth | **Bearer token loopback** (256 bits, `.jarvis/web.token`) | Discovery auto côté clients. Pas de mTLS tant que pas multi-utilisateur. |
| Bordures TUI | **Aucune** (borderless, inspiré OpenCode) | Lisibilité, look moderne. Whitespace > box-drawing. |
| Sort de l'HTMX legacy | **Supprimé en M6.S15** (-3000 lignes) | La SPA SolidJS feature-parity + flagship en plus. |

---

## License

MIT OR Apache-2.0
