# jarvis-mcp-server

Exposes the running Jarvis daemon as an **MCP (Model Context Protocol) server**
over stdio so external MCP clients (Claude Code, Codex, Cline, etc.) can drive
Jarvis as a sub-routine.

This is the mirror image of `jarvis-mcp`, which is an MCP *client* (Jarvis
talking to external MCP servers). Here Jarvis itself is the server.

## How it works

1. The MCP client (Claude Code, ...) spawns `jarvis-mcp-server` as a subprocess.
2. The server reads line-delimited JSON-RPC 2.0 requests from **stdin**, writes
   responses to **stdout**. All logs go to **stderr**.
3. On startup it connects to the running Jarvis daemon over gRPC
   (default `http://127.0.0.1:7777`), reusing the same bearer-token discovery
   as `jarvis-cli` (env `JARVIS_WEB_TOKEN`, else `<JARVIS_DATA_DIR>/web.token`,
   else `./.jarvis/web.token`).
4. Each MCP `tools/call` invokes one gRPC RPC and returns the JSON result
   as a text content block.

If the daemon is unreachable at startup the server still runs the protocol
handshake and surfaces a clear `isError: true` result on each `tools/call`.

## Exposed MCP tools

| Tool                  | Daemon RPC      | Arguments                                                            |
|-----------------------|-----------------|----------------------------------------------------------------------|
| `jarvis_ping`         | `Ping`          | none                                                                 |
| `jarvis_ask`          | `Ask` (stream)  | `{ prompt: string, model?: string }` — deltas collected into a string |
| `jarvis_submit_task`  | `SubmitTask`    | `{ goal: string, workdir?: string, routing?: string }` — returns `task_id` |
| `jarvis_get_task`     | `GetTask`       | `{ task_id: string }`                                                |
| `jarvis_list_tasks`   | `ListTasks`     | `{ all?: bool }` (default `false` — active tasks only)               |
| `jarvis_cancel_task`  | `CancelTask`    | `{ task_id: string }`                                                |
| `jarvis_status`       | `GetStatus`     | none                                                                 |

## Environment

| Var                  | Default                  | Purpose                                                       |
|----------------------|--------------------------|---------------------------------------------------------------|
| `JARVIS_DAEMON_URL`  | `http://127.0.0.1:7777`  | Daemon gRPC endpoint (full URL).                              |
| `JARVIS_DAEMON_ADDR` | unset                    | Alternative: bare `host:port` (auto-prefixed with `http://`). |
| `JARVIS_WEB_TOKEN`   | unset                    | Bearer token override; otherwise read from disk.              |
| `JARVIS_DATA_DIR`    | unset                    | Where to look for `web.token` (else `./.jarvis/`).            |
| `JARVIS_MCP_LOG`     | `info`                   | `tracing_subscriber` filter directive (e.g. `debug`).         |

## Manual smoke tests

The server does not require a running daemon for the handshake:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | jarvis-mcp-server
# → {"jsonrpc":"2.0","id":1,"result":{"capabilities":{"tools":{}},
#     "protocolVersion":"2024-11-05","serverInfo":{"name":"jarvis","version":"0.1.1"}}}

echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | jarvis-mcp-server
# → {"jsonrpc":"2.0","id":2,"result":{"tools":[ ... 7 entries ... ]}}

echo '{"jsonrpc":"2.0","id":3,"method":"ping"}' | jarvis-mcp-server
# → {"jsonrpc":"2.0","id":3,"result":{}}
```

With the daemon running, you can invoke a tool:

```bash
echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"jarvis_ping","arguments":{}}}' \
    | jarvis-mcp-server
# → {"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"{\n  \"version\": \"0.1.1\",\n  ..."}],"isError":false}}
```

## Wiring into an MCP client

Claude Code (`~/.config/claude/mcp.json`-style config, illustrative):

```json
{
  "mcpServers": {
    "jarvis": {
      "command": "jarvis-mcp-server",
      "env": {
        "JARVIS_DAEMON_URL": "http://127.0.0.1:7777"
      }
    }
  }
}
```

## Limitations

- `jarvis_ask`'s streaming deltas are collected into a single string before
  returning. The MCP `tools/call` spec doesn't currently expose chunked content
  to clients in a portable way.
- Only **tools** are exposed — not MCP `resources` or `prompts`.
- No auth on the stdio transport: this binary trusts whoever spawned it. The
  upstream gRPC connection is still bearer-token-authenticated.
- The server makes no attempt to map daemon errors to specific JSON-RPC error
  codes; everything that isn't a protocol-level error (unknown method,
  malformed params) is reported as a tool-level `isError: true` result.
