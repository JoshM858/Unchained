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

### Projects (implemented)

A project is a named workspace plus what belongs to it: its sessions, its
artifacts, its instructions file and, optionally, one notes vault. Projects
are stored in `<data_dir>/projects.json`. Every session belongs to exactly one
project; sessions written before projects existed are adopted into a project
named after their workspace.

- **Instructions.** `AGENTS.md`, or `CLAUDE.md` if there is none, is read from
  the workspace root at the start of every turn and given to the model.
- **Vault.** An absolute path to an Obsidian-style folder of Markdown notes.
  The agent reads notes with `search_notes` and `read_note`. Writes go through
  `write_note`, whose access is set per project and enforced by the daemon:
  `read-only` (default) refuses writes, `read-write-ask` asks each time, and
  `read-write` writes freely. The stricter of the vault mode and the session
  permission mode wins, so plan mode still refuses writes. Notes are walked
  with symlinks and hidden folders skipped, so nothing outside the vault root
  is listed or read.
- **Scratch.** Each session has a directory under `<data_dir>/scratch/` for
  temporary files. The agent writes there without approval, and the directory
  is outside every project. `promote_scratch` copies a file into the project
  and does need approval. Deleting a session deletes its scratch. A scratch
  directory idle for seven days is archived to
  `<data_dir>/archive/<session>-<ms>.tar.gz` with a JSON manifest, and its
  files are restored only when the user asks.
- **Artifacts.** `create_artifact` saves a named, versioned result (`html`,
  `svg`, `markdown` or `code`) under the project. Saving an existing name adds a
  version and keeps the older ones.

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

## Interface

A dense, dark HUD with one accent colour per theme and an alert colour
reserved for approvals and errors. It has three regions:

- **Left: projects and sessions.** Create a project from a workspace, optionally with a vault and its access mode. Under it, the project's instructions status and vault, then its sessions.
- **Centre: the conversation.** Tool calls and results as terminal blocks, approvals inline, and the composer. `/new` starts a session. Other slash commands are passed to the agent.
- **Right: workspace tabs.** Files (the project workspace), Notes (the vault, with search, rendered Markdown, `[[wikilinks]]` and backlinks), Scratch (this session's temporary files and the archive), Artifacts (rendered and versioned), and Changes.

A status strip across the top shows host telemetry from `GET /api/status`
(CPU, memory, disk, GPU, uptime), the permission mode, running sessions and
clients, and a pending-approvals count.

On a phone the regions become three views (projects, chat, workspace) behind a
bottom bar, with a badge when an approval is waiting.

The client talks to a backend object. `?demo` swaps the real HTTP and
WebSocket backend for an in-browser simulation that speaks the same event
protocol and exposes the same methods, which is what the GitHub Pages demo runs.

**Host console (concept).** `web/host.html` sketches what the host machine
shows on its own display: sessions across all workspaces, a merged activity
feed, model server state, linked devices, local approvals and a stop-all
control. It runs on simulated data only. Making it real needs the model
adapter, device pairing and a cross-session event stream.

Not yet built: git status badges in the file browser, diff view, the
terminal UI, and a `/plan` command (permission modes are daemon-wide, not per session).

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
- Artifacts are served from `/frames/<key>/...`, outside the API. The key is random per daemon start and is sent only to signed-in clients. The response allows scripts and styles inline but no network, forms or navigation, and the frame is sandboxed without same-origin access, so an artifact cannot reach the daemon.
- Vault and scratch paths are confined the same way as workspace paths, and vault paths are canonicalised when a project is created.

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
