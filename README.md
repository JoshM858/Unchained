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

Open <http://127.0.0.1:7878>, paste the token, create a project from the
workspace (add a vault folder if you have one), and start a session. With the
stub model, try `/ls`, `/read <file>`, `/write <file> <text>`, `/note <text>`
and `/artifact <name> html <markup>`. Writes pause for your approval; reload
the page mid-approval and the request is still waiting.

## Projects

A project is a workspace plus its sessions, artifacts, and an optional
Obsidian-style notes vault. Create one from the left panel.

- **Instructions.** An `AGENTS.md` or `CLAUDE.md` in the workspace root is read at the start of every turn.
- **Vault.** Point a project at a folder of Markdown notes. The agent can search and read them. Choose per project whether it may write: read only, ask before each write, or write freely. The Notes tab renders them, follows `[[wikilinks]]` and lists backlinks.
- **Scratch.** Each session has temporary files outside the project. Promote one into the project to keep it; that asks for approval. Idle scratch is archived after seven days and restored when you ask.
- **Artifacts.** Results the agent saves as versioned HTML, SVG, Markdown or code. HTML and SVG run in a sandboxed frame with no network access.

Slash commands in the composer are passed to the agent. The stub model
understands `/note`, `/find`, `/scratch`, `/scratch-ls`, `/promote`,
`/artifact` and `/context`. `/new` starts a session.

## Interface

The web UI is a dense, dark HUD with three regions: projects and sessions on
the left, the conversation in the centre, and tabs for files, notes, scratch,
artifacts and changes on the right. A status strip across the top carries host
telemetry and approvals. On a phone it becomes three views. Four colour
themes; a focus mode hides the side panels.

To see it without running anything, open `/?demo` on a running daemon, or
build the static demo that GitHub Pages publishes:

```bash
scripts/build-demo.sh _site
```

The demo has two tabs: the remote client, and a concept for the host
machine's own console. Both run on simulated data from `web/demo.js`; the
host console is not served by the daemon yet. The published copy lives at
<https://joshm858.github.io/unbound/>.

The `Interface demo` workflow (`.github/workflows/pages.yml`) publishes it.
One-time setup: in the repository, go to Settings → Pages and set **Source**
to **GitHub Actions**. Until that is set, the deploy step fails. The workflow
runs on pushes to `main` and to the feature branches it lists, and on demand
from the Actions tab.

Pages serves only the static demo. `client.html` redirects to demo mode there,
because nothing is listening for a daemon on that host.

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
| `crates/core` | Agent loop, tools, permission engine, workspaces, projects, notes, artifacts, scratch and archives, session store |
| `crates/server` | Gateway: auth, HTTP and WebSocket API, embedded web UI |
| `crates/cli` | The `unchained` binary (`serve`, `token`) |
| `web/` | The web client, the Markdown renderer, the simulated demo backend and the host-console concept. Plain HTML, CSS and JS; no build step |
| `scripts/build-demo.sh` | Assembles the static interface demo |
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
