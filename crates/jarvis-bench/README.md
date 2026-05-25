# jarvis-bench

A reproducible benchmark harness for Jarvis. Replays a declarative YAML
suite of tasks against a config/model and emits a JSON scorecard.

```
jarvis-bench run --suite crates/jarvis-bench/benches/basic.yaml \
                 --output scorecard.json \
                 [--config jarvis.toml] \
                 [--routing auto] \
                 [--task <id>] \
                 [--keep-tempdir]
```

The bench is a pure consumer of the public `jarvis_agent::run_agent` API.
It neither modifies the daemon nor touches the agent loop, so it can be
re-run on any commit without coupling.

## Adding a task

Append an entry under `tasks:` in any suite YAML:

```yaml
- id: my-task             # unique within the suite
  goal: "Plain-English instruction for the agent."
  fixtures:               # optional; relative path -> file content
    "src/main.rs": |
      fn main() {}
  success:
    type: file_contains   # see "Success criteria" below
    path: src/main.rs
    pattern: "println!"
  max_steps: 8            # optional override of defaults.max_steps
  timeout_s: 60           # optional override of defaults.timeout_s
```

Each task gets its own freshly-created tempdir. The agent's `workdir`
is rooted there; the criterion is evaluated against the same path after
the loop terminates.

## Success criteria

Three predicates are supported in v0:

- `file_equals { path, content }`
  File exists and (trimmed) content equals (trimmed) `content`.
- `file_contains { path, pattern }`
  File exists and contains `pattern` as a substring.
- `shell_exits_zero { cmd }`
  Command run in the workdir (via `cmd /C` on Windows, `sh -c` on unix)
  exits 0.

## Outcomes

For every task the scorecard records one of:

| outcome | meaning                                                            |
| ------- | ------------------------------------------------------------------ |
| pass    | The agent finished and the success criterion held.                  |
| fail    | The agent finished but the criterion did not hold.                  |
| timeout | The agent did not return within `timeout_s`.                        |
| error   | The agent loop returned an error, or the harness failed to set up.  |

## Interpreting the JSON scorecard

```jsonc
{
  "suite": "basic",
  "started_at": "2026-05-25T14:30:00Z",
  "finished_at": "...",
  "config_summary": {
    "model": "local:gemma",
    "routing": "auto",
    "sandbox": "native"
  },
  "tasks": [
    {
      "id": "write-hello",
      "outcome": "pass",
      "steps_taken": 2,           // count of EventKind::Decision events
      "duration_ms": 4321,
      "tokens_in": 1234,          // sum of Observation/turn_usage events
      "tokens_out": 56,
      "cost_usd": 0.0,            // v0: always 0 (pricing not yet wired)
      "failure_reason": null
    }
  ],
  "summary": {
    "total": 3, "pass": 3, "fail": 0, "timeout": 0, "error": 0,
    "pass_rate": 1.0,
    "total_duration_ms": 12000,
    "total_tokens_in": 4000, "total_tokens_out": 200,
    "total_cost_usd": 0.0
  }
}
```

## Stub mode

If no providers are configured (no `--config`, no `jarvis.toml` in the
current directory, no `HERMES_LOCAL_*` env vars), the bench falls back to
a built-in `StubProvider` that immediately replies with
`{"action":"done","message":"stub provider — no LLM was actually called"}`.

Stub mode is for validating the harness itself. The agent loop terminates
after one step without doing any work, so any task whose success criterion
requires a non-empty workdir will record `outcome: "fail"`. The bench still
produces a fully-formed scorecard, exit code 0.

## Real-LLM mode

Wire a config like the daemon's:

```toml
# jarvis.toml
[providers.local.gemma]
url = "http://localhost:11434/v1"
model = "gemma-4-26b-it"
priority = 10

[providers.local.gemma.capabilities]
ctx_len = 32768
tool_calls = true
```

Then `jarvis-bench run --suite ... --config jarvis.toml --output ...`.
The bench builds the same `LlmPool`/`ToolRegistry` shape the daemon uses
(minus MCP, sub-agents, hooks, validation — kept out for v0
reproducibility).

## Limitations (v0)

- `cost_usd` is hard-coded to `0.0`. Wire `jarvis_llm::pricing::estimate_usd`
  once token attribution lands per-task.
- No telemetry beyond ledger-derived counts; sub-task fan-out is not
  counted (sub-agent tool is intentionally not registered).
- Sandbox is always `Native`. Docker/WSL2 backends are out of scope for
  v0 to keep the harness portable across CI runners.
- Stub provider mode produces uniform "fail" outcomes for non-trivial
  tasks — by design, it only exercises the harness plumbing.
