# Unchained

An open-source coding agent that lives on a machine you own and never logs off.

Install it on a workstation or server that holds your repositories and your
model. It runs as a persistent service: it reads and edits files, drives Git,
runs commands, and carries long tasks through whether or not you are
connected. From any browser you pick up the conversation, review what it did,
approve what it wants to do next, and hand it more work.

- **Your hardware.** Code, model and logs stay on your machine. No account, no telemetry.
- **Open-weight models first.** The harness adapts to the model, not the other way round.
- **Control lives in the harness.** Permissions, a sandbox and a rewindable log make autonomy safe, whatever model is driving.
- **Always on.** Sessions belong to the daemon. Closing the browser stops nothing.

> **Status: scaffold.** The daemon, protocol, permission engine, session log
> and web UI work end to end. There is no model adapter, shell tool or sandbox
> yet; a built-in stub stands in for the model. See [Roadmap](#roadmap).

## Try it

```bash
cargo run -p unchained -- serve --workspace /path/to/a/project
```

```bash
cargo run -p unchained -- token
```

Open <http://127.0.0.1:7878>, paste the token, start a session. With the stub
model, try `/ls`, `/read <file>` and `/write <file> <text>`. Writes pause for
your approval; reload the page mid-approval and the request is still waiting.

## Reaching it from elsewhere

The daemon is a service that changes files on request, so it listens on
localhost only and refuses any other address unless you pass `--allow-remote`.
In order of preference:

1. **Private network** (WireGuard, Tailscale): bind the overlay address. The service never appears on the public internet.
2. **SSH tunnel**: `ssh -L 7878:127.0.0.1:7878 your-host`, then browse to localhost.
3. **Reverse proxy with TLS**: only if you need a public hostname, and only with TLS in front. The daemon itself speaks plain HTTP.

## Layout

| Path | What it is |
|---|---|
| `crates/protocol` | Wire types: the session event log and client messages |
| `crates/core` | Agent loop, tools, permission engine, workspaces, session store |
| `crates/server` | Gateway: auth, HTTP and WebSocket API, embedded web UI |
| `crates/cli` | The `unchained` binary (`serve`, `token`) |
| `web/` | The web client. Plain HTML, CSS and JS; no build step |
| `docs/DESIGN.md` | Vision, architecture, security model, roadmap |

## Permission modes

| Mode | Reads | File writes | Commands, network |
|---|---|---|---|
| `plan` | allow | deny | deny |
| `ask` (default) | allow | ask | ask |
| `auto-edit` | allow | allow | ask |
| `full-auto` | allow | allow | allow |

An unanswered request is denied after 15 minutes.

## Roadmap

1. Model adapter for OpenAI-compatible endpoints (Ollama, llama.cpp, vLLM), with per-model profiles
2. Patch-based edit tool and Git tools
3. OS sandbox, then the shell tool
4. Checkpoints and rewind
5. Device pairing and per-device revocation, replacing the single token
6. Notifications for approvals and finished tasks
7. Task queue, worktrees, scheduled runs

## Development

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

## License

Not chosen yet.
