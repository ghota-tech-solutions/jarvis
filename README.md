# Jarvis

> Système d'agents de codage autonome et continu, à la qualité institutionnelle, en Rust.

**Statut : `v0.1.0-dev` — M5 livré · daemon + agent + sandbox + multi-modèles**

Jarvis est conçu pour **travailler en continu** sur les tâches que tu lui donnes, plutôt
que de répondre tour par tour comme un chatbot. Il dépasse OpenCode, Claude Code,
Hermes et Codex sur trois axes que personne d'autre n'attaque correctement :

1. **Ledger event-sourced** (SQLite, append-only) comme mémoire primaire — pas le
   contexte LLM. L'agent **interroge son passé** (`failures_for(path)`,
   `recent_decisions(task)`) avant d'agir, et son travail survit aux redémarrages.
2. **Registry multi-modèles** avec capabilities probées + quarantaine automatique :
   tu ajoutes un modèle en éditant `jarvis.toml`, zéro code. Le routeur choisit par
   capability requise et bascule tout seul quand un modèle galère.
3. **Daemon + clients gRPC** : tu peux fermer la TUI, le travail continue. La TUI,
   le CLI, et plus tard une Web UI partagent le même état.

---

## Architecture

```
                   ┌─────────────────────────────────┐
                   │         jarvis-daemon           │
                   │  (gRPC sur 127.0.0.1:7777)      │
                   │                                 │
        ┌──────────┤  Orchestrator                   ├──────────┐
        │          │  ├─ LlmPool (registry+probe+    │          │
   ┌────▼────┐     │  │   quarantine+routing)        │     ┌────▼────┐
   │ jarvis  │     │  ├─ Agent loop (plan-act-       │     │ jarvis  │
   │  -cli   │     │  │   observe, JSON tool calls)  │     │  -tui   │
   └─────────┘     │  ├─ Sandbox (Native | Docker)   │     └─────────┘
                   │  ├─ Worktree mgr (git2)         │
                   │  └─ Ledger (SQLite WAL,         │
                   │      append-only, broadcast)    │
                   └─────────────────────────────────┘
```

### Crates

| Crate | Rôle |
|---|---|
| `jarvis-core` | Types, traits, erreurs. Pas d'I/O. |
| `jarvis-api` | Proto gRPC + client/server générés par tonic. |
| `jarvis-config` | Loader TOML + env (figment, interpolation `${VAR}` et `$$` escape). |
| `jarvis-ledger` | SQLite WAL, event store append-only, broadcast bus. |
| `jarvis-llm` | `LlmProvider` trait, OpenAI-compat client, `ModelRegistry`, `LlmPool`. |
| `jarvis-tools` | `Tool` trait + builtins (fs_read, fs_write, shell via sandbox). |
| `jarvis-sandbox` | `Sandbox` trait, `NativeSandbox`, `DockerSandbox` (bollard), `WorktreeManager`. |
| `jarvis-agent` | Boucle plan-act-observe, parser JSON tolérant. |
| `jarvis-daemon` | Binaire long-running, orchestrateur, expose gRPC. |
| `jarvis-cli` | Binaire `jarvis` — client gRPC scriptable. |
| `jarvis-tui` | Binaire `jarvis-tui` — front-end ratatui, look OpenCode. |

DAG strict, vérifié à chaque PR : `core` est leaf ; les clients (`cli`, `tui`)
ne dépendent que de `api` + `core`.

---

## Quick start

```powershell
# 1. Cloner et builder
cargo build --release --workspace

# 2. Configurer
cp .env.example .env                 # remplir HERMES_LOCAL_*, DEEPSEEK_API_KEY
cp jarvis.toml.example jarvis.toml   # ajuster modèles, sandbox, routing

# 3. Démarrer le daemon (shell 1)
$env:HERMES_LOCAL_URL='http://192.168.1.14:8000/v1'
$env:HERMES_LOCAL_MODEL='gemma-4-26B-A4B-it-MLX-4bit'
$env:HERMES_LOCAL_API_KEY='azer'
$env:DEEPSEEK_API_KEY='sk-...'        # optionnel ; ou 'disabled' pour local-only
.\target\release\jarvis-daemon.exe run

# 4. Utiliser (shell 2) — au choix
.\target\release\jarvis-tui.exe       # interface graphique terminal
.\target\release\jarvis.exe ping      # CLI scriptable
```

---

## Configuration (`jarvis.toml`)

```toml
[daemon]
addr = "127.0.0.1:7777"
data_dir = ".jarvis"

# --- Modèles locaux (multi-entrées) ---
[providers.local.gemma]
url      = "${HERMES_LOCAL_URL}"
model    = "${HERMES_LOCAL_MODEL}"
api_key  = "${HERMES_LOCAL_API_KEY}"
priority = 10
capabilities = { ctx_len = 128000, tool_calls = true, json_schema = true, vision = false }

# Ajoute autant d'entrées que tu veux : qwen, deepseek-flash local, modèle vision…
# [providers.local.qwen]
# url = "http://192.168.1.14:8001/v1"
# model = "qwen3-32B-instruct"
# priority = 8
# capabilities = { ctx_len = 32000, tool_calls = true, json_schema = true, vision = false }

# --- Modèles remote ---
[providers.remote.deepseek_pro]
url      = "https://api.deepseek.com/v1"
model    = "deepseek-chat"
api_key  = "${DEEPSEEK_API_KEY:-disabled}"
priority = 7
cost_per_mtok_in  = 0.27
cost_per_mtok_out = 1.10
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
default_backend    = "native"              # native | docker
default_net_policy = "egress_only"         # none | egress_only | full  (docker uniquement)
docker_image       = "alpine:3.20"
docker_memory      = "2g"
docker_cpus        = 2.0
docker_autopull    = true
```

Le loader supporte l'interpolation `${VAR}` et `${VAR:-default}` avec escape `$$`.
Les commentaires sont ignorés par l'interpolateur.

---

## CLI — `jarvis`

```
# Santé + statut
jarvis ping
jarvis status                                  # liste des modèles + état

# LLM brut (one-shot, bypass agent)
jarvis ask "Liste 3 crates Rust pour les acteurs"

# Tâches autonomes
jarvis task add "Refactor src/ to use anyhow"  # workdir = cwd par défaut
jarvis task add --workdir C:\proj\app --watch "Ajoute une route POST /users"
jarvis task add --sandbox docker --net egress_only --worktree --watch \
   "Run cargo test and fix any failures"
jarvis task add --model local:fallback "..."   # force un modèle précis
jarvis task add --routing remote_only "..."    # remote uniquement
jarvis task add --require vision "Décris cette image"

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
| `--sandbox native\|docker` | Backend d'exécution. Défaut = config. |
| `--net none\|egress_only\|full` | Politique réseau (Docker uniquement). |
| `--worktree` | Crée un git worktree isolé si workdir est un repo. |
| `--base-ref <ref>` | Ref Git de base pour le worktree (défaut `HEAD`). |
| `--routing <policy>` | `auto`, `local_only`, `remote_only`, ou `model:<name>`. |
| `--model <name>` | Raccourci pour `--routing model:<name>`. |
| `--require <cap>` | Capacité requise (répétable) : `tool_calls`, `json_schema`, `vision`. |
| `--watch` | Streame les events live après soumission. |

---

## TUI — `jarvis-tui`

Interface borderless inspirée d'OpenCode :

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
│ → fs_write src/auth/jwt.rs                   │ ● qwen · local         │
│   ✓ wrote 2103 bytes                         │ ● deepseek_pro · remote│
│                                              │                        │
│                                              │ ▼ Daemon               │
│                                              │ v0.1.0 · up 1832s      │
├──────────────────────────────────────────────┴────────────────────────┤
│ jarvis · http://127.0.0.1:7777    j/k navigate · :cmd · ? help        │
│ ❯ type to send  ·  :cancel  :all  :refresh  :theme  :q                │
└────────────────────────────────────────────────────────────────────────┘
```

**Input always-visible** : tape un goal et `Enter` pour soumettre une tâche. Préfixe
`:` pour les commandes (slash palette).

| Touche / commande | Effet |
|---|---|
| `Enter` (texte) | Soumet une nouvelle tâche dans le cwd du process |
| `:new <goal>` | Idem mais explicite |
| `:cancel` | Annule la tâche sélectionnée (avec confirmation) |
| `:refresh` (ou `r`) | Force un refresh |
| `:all` (ou `a`) | Toggle actives ↔ toutes |
| `:theme dark\|light` | Change le thème |
| `:q` (ou `q`, `Esc` sur input vide) | Quitter |
| `j`/`k`, `↓`/`↑` | Naviguer dans les tâches |
| `c` | Cancel la tâche sélectionnée |
| `?` | Aide |

Tu peux **tuer la TUI** à tout moment, le daemon continue le travail. Relance
`jarvis-tui` pour rejoindre l'état (gRPC re-sync + replay des events).

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
`error`, `verdict`, `spawn`, `heartbeat`.

---

## Tests

```powershell
cargo test --workspace --lib              # 44 tests, ~10s
cargo clippy --workspace -- -D warnings   # zéro warning
```

Smoke tests manuels documentés dans le commit message de chaque jalon
(`git log`). Pour un guide pas-à-pas, voir la conversation Claude qui a généré
chaque milestone.

---

## Roadmap

- [x] **M1** — squelette daemon + CLI + LLM local
- [x] **M2** — ledger event-sourcing + boucle agent
- [x] **M3** — sandbox Native + Docker + worktrees + parallélisme
- [x] **M4** — TUI ratatui multi-pane
- [x] **M4.5** — refonte UI borderless inspirée d'OpenCode
- [x] **M5** — registry multi-modèles + routage capability-aware + quarantaine
- [ ] **M6** — Web UI (axum + HTMX, dashboard multi-projets) + MCP client
- [ ] *future* — Reviewer agent dédié, BudgetActor avec hard-cap, re-probe périodique,
      LSP hookup, autocomplete `:` palette, plus de thèmes.

Plan complet et risques : `~/.claude/plans/je-souhaite-cr-er-dynamic-moore.md`.

---

## Décisions structurantes

| Question | Réponse | Pourquoi |
|---|---|---|
| Langage | **Rust** | Performance, type safety, écosystème async mature. Pas de DSL maison — Rust + TOML + Rhai en option suffit. |
| Architecture process | **Daemon + clients** | La TUI peut être tuée sans interrompre le travail nocturne. Multi-clients via gRPC. |
| Mémoire de l'agent | **Ledger append-only** (SQLite) | Le contexte LLM est jetable ; les attempts/decisions sont permanentes et interrogeables. |
| Orchestration | **Supervisor/worker** (Tokio tasks, Ractor plus tard) | Pattern qui a shippé en prod (Codex 2.0, Devin). Pas de swarm. |
| Isolation | **2 sandboxes : Native + Docker** (choix par tâche) | Native = fast debug. Docker = isolation kernel-level pour le code nocturne. |
| Politique réseau | **3 modes : none / egress_only / full** | `egress_only` est le défaut sensé (permet `cargo build`, `npm install`). |
| Multi-modèles | **Registry** (N local + N remote), routing capability-aware | Pas de lock-in. Ajout = éditer TOML, zéro code. Failover auto via quarantaine. |
| Bordures TUI | **Aucune** (borderless, inspiré OpenCode) | Lisibilité, look moderne. Whitespace > box-drawing. |

---

## License

MIT OR Apache-2.0
