# Unchained: design

## Vision

Unchained is an open-source coding agent that lives on a machine you own and
never logs off. It runs as a persistent service next to your repositories and
your model: it reads and edits files, drives Git, runs commands, and carries
long tasks to completion whether or not you are connected. From any browser
you open a clean interface, pick up the conversation, review diffs, approve
actions and hand it more work. It is built for open-weight models first.
Every action is governed by permissions you define, confined by a sandbox,
and recorded in a log you can read and rewind. No vendor cloud sits in the
middle.

## Principles

- **Open source, local-first.** No account, no telemetry, no hidden prompts.
- **Open-weight models are the primary target.** Prompts, tool protocol and edit format adapt per model.
- **Safety comes from the harness, not the model.** Permissions, sandbox and checkpoints hold regardless of what the model does.
- **The daemon owns the work.** Clients are views.
- **Private by default.** Remote reach is something you opt into.

## Architecture

```
 Browser / phone      Terminal UI        CLI / scripts
       └────────────────┬──────────────────┘
          WebSocket + HTTP (authenticated; TLS or private network)
 ┌──────────────────────▼──────────────────────────┐
 │ unchained daemon (host machine)                 │
 │  Gateway: auth, origin checks, static web UI    │
 │  Session manager: event log, replay, restore    │
 │  Agent loop ─ Model adapter ─► local LLM server │
 │  Tool registry ─► Permission engine ─► Sandbox  │
 │  Checkpoints · Audit log                        │
 └─────────────────────────────────────────────────┘
```

### Sessions are event logs

A session is an append-only list of events (`crates/protocol`), mirrored to
`<data_dir>/sessions/<id>/events.jsonl`. Sequence numbers are gapless. A
client connects with `?after=<seq>`, receives everything later, then follows
live. Replay, resume, multi-device sync and restore after a restart are all
the same mechanism. If the daemon stops mid-turn, the restored session
records that and accepts new messages.

### Agent loop

`crates/core/src/session.rs`. A user message starts a turn. The loop asks the
model for the next step, emits any text, and runs each requested tool through
the permission engine. It ends when the model requests no tools, on
interrupt, or after a step limit. One turn runs per session at a time.

### Permissions

`crates/core/src/permissions.rs`. Each tool declares an effect: read, write,
execute or network. A call resolves to allow, ask or deny. Explicit rules
match first, in order; otherwise the session mode decides. "Ask" pauses the
turn, emits a request to every attached client, and waits. No answer within
the timeout is a denial.

Planned: rules matching on paths and command patterns, layered user →
workspace → session, and a per-workspace unattended policy.

### Workspaces

The daemon is started with an explicit list of directories. Sessions can only
be created in those, and every path a tool receives is resolved against the
workspace root, rejecting `..` and symlink escapes. This is an in-process
check, not a sandbox.

### Sandbox (planned)

The hard boundary for spawned commands: Landlock and seccomp or bubblewrap on
Linux, Seatbelt on macOS. Writes confined to the workspace, network off
unless granted. **The shell tool ships with the sandbox, not before it.** In
service mode the sandbox is mandatory.

### Model adapter (planned)

One internal message and tool-call format; adapters for OpenAI-compatible
endpoints and hosted APIs. A per-model profile selects:

- tool-call protocol: native function calling, grammar-constrained JSON, or a parsed text format
- edit format: search/replace blocks, unified diff, or whole file
- context budget and sampling settings

A `bench` command runs a conformance suite against a model and writes its
profile. On a single-GPU host, a queue schedules inference across sessions.

### Context (planned)

A token budget split across project instructions, a repo map, history and
tool results, with older turns compacted to summaries. Local context windows
make this matter more than it does for hosted models.

### Checkpoints (planned)

A snapshot on a shadow Git ref before each mutation; one command rewinds.

## Remote access and security model

A service that edits files and runs commands on request is remote code
execution by design. The design treats that as the central fact.

**Implemented**

- Binds `127.0.0.1` by default; a non-loopback address requires `--allow-remote`.
- A 256-bit access token, generated on first run, stored with mode 0600.
- Every API route except `health` and `login` requires the token (Bearer header, or an HttpOnly, SameSite=Strict cookie). Comparison is constant-time.
- Requests whose `Origin` does not match `Host` are refused, including WebSocket handshakes.
- The web UI is served with a `default-src 'self'` content security policy and renders all server text as text.
- Sessions exist only in registered workspaces.

**Planned**

- Device pairing: a one-time code shown on the host enrols a browser, which then holds a passkey or revocable device token. Replaces the shared token.
- Device list and revocation; every event records the device that caused it.
- Notifications (web push, webhook) for approval requests and finished tasks.
- Global stop for all sessions.
- Run as a dedicated unprivileged user under systemd or launchd.

**Recommended ways to reach the daemon**

1. A private overlay network (WireGuard, Tailscale).
2. An SSH tunnel.
3. A reverse proxy with TLS, for a public hostname. The daemon speaks plain HTTP, so never expose it directly.

**Deferred**

- A hosted or self-hosted relay for NAT traversal.
- An interactive web terminal. v1 shows command output read-only.

## Feature set for the first serious version

- Agent loop with interrupt and steer
- File tools: read, search, glob, write, patch-based edit
- Shell tool with streamed output, timeouts and background processes
- Git: status, diff, branch, commit, worktrees; no push without an explicit rule
- Permission engine with path and command rules
- OS sandbox
- Checkpoints and rewind
- Model adapters and profiles
- Context management and project instructions file
- Persistent sessions, task queue, parallel sessions in worktrees
- Browser diff review with per-hunk accept and reject
- Notifications and remote approvals
- Scheduled and triggered runs
- MCP client support and lifecycle hooks
- Web UI, terminal UI and headless mode over one protocol

## Differentiation

- **Open-weight first.** Tested profiles and a conformance bench, not best-effort local support.
- **Self-hosted and always on.** The convenience of a cloud agent with nothing leaving your machine.
- **Harness-level safety.** Sandbox and checkpoints make full autonomy reasonable with any model.
- **Nothing hidden.** Prompts are plain files and every request is logged.
- **Small, embeddable core.** A library with a documented event protocol.

## Tone and positioning

- Lead line: "Your own always-on coding agent, on your own hardware."
- Voice: terse and technical. Show commands and transcripts, not adjectives.
- Be upfront about the security model; for a tool like this, candour is the selling point.
- Publish per-model benchmark results, failures included.
- Avoid hype words, "AI teammate" language and imagery of taking over machines.

## Open decisions

- **License.** Not chosen.
- **Name.** "Unchained" has not been checked against package registries or trademarks.
- **Web UI stack.** Currently plain JS with no build step; diff review may justify a framework later.
