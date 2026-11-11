// Unchained web client. No build step: these files are embedded in the
// daemon binary and served as-is. All server-supplied text is inserted with
// textContent or the Markdown renderer, never as HTML.
//
// The client talks to a "backend" object. The real one below uses the
// daemon's HTTP and WebSocket API; `?demo` swaps in the in-browser
// simulation from demo.js so the interface can be shown with no daemon.
"use strict";

const { $, text, bar } = HUD;

async function parse(response) {
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error || `request failed (${response.status})`);
  return body;
}

const realBackend = {
  demo: false,
  async request(path, options = {}) {
    const response = await fetch("api" + path, {
      credentials: "same-origin",
      headers: options.body ? { "content-type": "application/json" } : {},
      ...options,
    });
    if (response.status === 401) {
      showLogin();
      throw new Error("unauthorized");
    }
    return response;
  },
  async login(token) {
    const response = await fetch("api/login", {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token }),
    });
    return response.ok;
  },
  async logout() {
    await fetch("api/logout", { method: "POST", credentials: "same-origin" });
  },
  async workspaces() {
    return parse(await this.request("/workspaces"));
  },
  async status() {
    return parse(await this.request("/status"));
  },
  async projects() {
    return parse(await this.request("/projects"));
  },
  async createProject(body) {
    return parse(await this.request("/projects", { method: "POST", body: JSON.stringify(body) }));
  },
  async updateProject(id, body) {
    return parse(await this.request(`/projects/${encodeURIComponent(id)}`, { method: "PATCH", body: JSON.stringify(body) }));
  },
  async instructions(id) {
    return parse(await this.request(`/projects/${encodeURIComponent(id)}/instructions`));
  },
  async notes(id) {
    return parse(await this.request(`/projects/${encodeURIComponent(id)}/notes`));
  },
  async note(id, path) {
    const q = new URLSearchParams({ path });
    return (await parse(await this.request(`/projects/${encodeURIComponent(id)}/note?${q}`))).text;
  },
  async backlinks(id, path) {
    const q = new URLSearchParams({ path });
    return parse(await this.request(`/projects/${encodeURIComponent(id)}/backlinks?${q}`));
  },
  async searchNotes(id, query) {
    const q = new URLSearchParams({ q: query });
    return parse(await this.request(`/projects/${encodeURIComponent(id)}/search?${q}`));
  },
  async artifacts(id) {
    return parse(await this.request(`/projects/${encodeURIComponent(id)}/artifacts`));
  },
  async artifact(id, name, version) {
    const q = version ? `?${new URLSearchParams({ version: String(version) })}` : "";
    return parse(await this.request(`/projects/${encodeURIComponent(id)}/artifacts/${encodeURIComponent(name)}${q}`));
  },
  async sessions(project) {
    const q = new URLSearchParams({ project });
    return parse(await this.request(`/sessions?${q}`));
  },
  async createSession(project) {
    return parse(await this.request("/sessions", { method: "POST", body: JSON.stringify({ project }) }));
  },
  async deleteSession(id) {
    const response = await this.request(`/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });
    if (!response.ok) await parse(response);
  },
  async scratchList(id, path) {
    const q = new URLSearchParams({ path });
    return parse(await this.request(`/sessions/${encodeURIComponent(id)}/scratch?${q}`));
  },
  async scratchRead(id, path) {
    const q = new URLSearchParams({ path });
    return (await parse(await this.request(`/sessions/${encodeURIComponent(id)}/scratch/file?${q}`))).text;
  },
  async archives() {
    return parse(await this.request("/archives"));
  },
  async restoreArchive(name) {
    return parse(await this.request(`/archives/${encodeURIComponent(name)}/restore`, { method: "POST" }));
  },
  async files(workspace, path) {
    const q = new URLSearchParams({ path });
    const response = await this.request(`/workspaces/${encodeURIComponent(workspace)}/files?${q}`);
    return response.ok ? response.json() : [];
  },
  async readFile(workspace, path) {
    const q = new URLSearchParams({ path });
    const response = await this.request(`/workspaces/${encodeURIComponent(workspace)}/file?${q}`);
    const body = await response.json();
    return response.ok ? body.text : `Cannot open this file: ${body.error}`;
  },
  connect(id, after, handlers) {
    const url = new URL(`api/sessions/${encodeURIComponent(id)}/ws?after=${after}`, location.href);
    url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(url);
    socket.onopen = handlers.onopen;
    socket.onmessage = (m) => handlers.onevent(JSON.parse(m.data));
    socket.onclose = handlers.onclose;
    return {
      send: (message) => socket.readyState === WebSocket.OPEN && socket.send(JSON.stringify(message)),
      close() {
        socket.onclose = null;
        socket.close();
      },
    };
  },
};

const backend = new URLSearchParams(location.search).has("demo") ? window.UnchainedDemo : realBackend;

const state = {
  projects: [],
  frameBase: "",
  projectId: null,
  instructions: null,
  notes: [],
  artifacts: [],
  workspaces: [],
  sessions: [],
  current: null,
  link: null,
  online: false,
  lastSeq: 0,
  running: false,
  retry: null,
  cwd: ".",
  scratchPath: ".",
  tab: "files",
  calls: new Map(), // call_id -> { tool, path }
  changed: new Set(),
  asks: new Map(), // call_id -> { entry, tool }
};

const projectById = (id) => state.projects.find((p) => p.id === id) || null;
const currentProject = () => projectById(state.projectId);
const currentSession = () => state.sessions.find((s) => s.id === state.current) || null;
const short = (id) => id.slice(0, 8);

function showLogin() {
  closeLink();
  $("app").hidden = true;
  $("login").hidden = false;
  $("token").focus();
}

async function boot() {
  try {
    state.workspaces = await backend.workspaces();
    const listing = await backend.projects();
    state.projects = listing.projects;
    state.frameBase = listing.frame_base || "";
  } catch {
    return;
  }
  $("login").hidden = true;
  $("app").hidden = false;
  $("np-workspace").replaceChildren(
    ...state.workspaces.map((w) => Object.assign(document.createElement("option"), { value: w.name, textContent: w.name, title: w.root })),
  );
  renderProjects();
  const remembered = HUD.store("unchained-project");
  const pick = projectById(remembered) || state.projects[0] || null;
  if (pick) await selectProject(pick.id);
  else renderProjectPanel();
  pollStatus();
}

// ---- projects ----

function renderProjects() {
  $("project-list").replaceChildren(
    ...state.projects.map((p) => {
      const li = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      button.className = p.id === state.projectId ? "active" : "";
      button.title = p.vault ? `${p.workspace} · vault ${p.vault}` : p.workspace;
      button.textContent = p.name;
      button.append(Object.assign(document.createElement("small"), { textContent: `${p.workspace}${p.vault ? " · vault" : ""}` }));
      button.addEventListener("click", () => selectProject(p.id));
      li.append(button);
      return li;
    }),
  );
  if (!state.projects.length) {
    $("project-list").replaceChildren(Object.assign(document.createElement("li"), { className: "muted", textContent: "No projects yet." }));
  }
}

async function selectProject(id) {
  const project = projectById(id);
  if (!project) return;
  if (state.projectId !== id) {
    state.projectId = id;
    HUD.store("unchained-project", id);
    state.notes = [];
    state.artifacts = [];
    state.scratchPath = ".";
  }
  renderProjects();
  renderProjectPanel();
  const workspace = project.workspace;
  await Promise.all([loadInstructions(), loadNotes(), loadArtifacts()]);
  if (state.projectId !== id) return;
  await refreshSessions();
  const current = currentSession();
  if (current && current.project !== id) closeSession();
  if (!currentSession() && state.sessions.length) openSession(state.sessions[0].id);
  else if (!currentSession()) closeSession();
  loadFiles(".", workspace);
}

async function loadInstructions() {
  const p = currentProject();
  if (!p) return;
  try {
    state.instructions = await backend.instructions(p.id);
  } catch {
    state.instructions = null;
  }
  renderProjectPanel();
}

function renderProjectPanel() {
  const p = currentProject();
  if (!p) return;
  text("p-name", p.name);
  text("p-workspace", p.workspace);
  $("p-workspace").title = state.workspaces.find((w) => w.name === p.workspace)?.root || "";
  text("p-vault", p.vault ? p.vault.split("/").filter(Boolean).slice(-2).join("/") : "none");
  $("p-vault").title = p.vault || "";
  $("p-mode").value = p.vault_mode;
  $("p-mode").disabled = !p.vault;
  $("p-vault-detach").hidden = !p.vault;
  $("p-vault-path").placeholder = p.vault ? "Replace the vault folder" : "/path/to/vault";
  const i = state.instructions;
  text("p-instructions", i && i.file ? `${i.file} (${i.text.length} chars)` : "none: add AGENTS.md or CLAUDE.md");
}

function showProjectError(message) {
  $("p-vault-error").hidden = !message;
  text("p-vault-error", message || "");
}

async function changeProject(body) {
  try {
    const updated = await backend.updateProject(state.projectId, body);
    state.projects = state.projects.map((p) => (p.id === updated.id ? updated : p));
    showProjectError("");
    renderProjects();
    renderProjectPanel();
    await loadNotes();
  } catch (e) {
    showProjectError(e.message);
    renderProjectPanel();
  }
}

// ---- sessions ----

async function refreshSessions() {
  if (!state.projectId) return;
  try {
    state.sessions = await backend.sessions(state.projectId);
  } catch {
    return;
  }
  renderSessions();
}

function renderSessions() {
  text("s-count", String(state.sessions.length));
  $("session-list").replaceChildren(
    ...state.sessions.map((s) => {
      const li = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      const active = s.id === state.current;
      button.className = `${active ? "active" : ""} ${(active ? state.running : s.running) ? "running" : ""}`.trim();
      button.textContent = `session ${short(s.id)}`;
      button.append(Object.assign(document.createElement("small"), { textContent: new Date(s.created_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) }));
      button.addEventListener("click", () => openSession(s.id));
      const remove = Object.assign(document.createElement("button"), {
        type: "button",
        className: "x",
        textContent: "×",
        title: "Delete session and its scratch files",
        disabled: active ? state.running : s.running,
      });
      remove.setAttribute("aria-label", `Delete session ${short(s.id)}`);
      remove.addEventListener("click", () => removeSession(s.id));
      li.append(button, remove);
      return li;
    }),
  );
}

async function removeSession(id) {
  if (!confirm("Delete this session and its scratch files? Promoted project files stay.")) return;
  try {
    await backend.deleteSession(id);
  } catch (e) {
    addEntry("error", "error", e.message);
    return;
  }
  if (state.current === id) closeSession();
  await refreshSessions();
  loadScratch();
}

async function newSession() {
  if (!state.projectId) return;
  const session = await backend.createSession(state.projectId);
  await refreshSessions();
  openSession(session.id);
}

function closeLink() {
  clearTimeout(state.retry);
  if (state.link) state.link.close();
  state.link = null;
  state.online = false;
}

function setStatus(message, cls) {
  const el = $("status");
  el.textContent = message;
  el.className = cls || "";
}

function closeSession() {
  closeLink();
  state.current = null;
  state.lastSeq = 0;
  state.calls.clear();
  state.changed.clear();
  state.asks.clear();
  setRunning(false);
  $("log").replaceChildren();
  $("input").disabled = true;
  text("title", "No session");
  setStatus("offline");
  renderChanges();
  renderApprovals();
  renderSessions();
  loadScratch();
}

function openSession(id) {
  closeLink();
  state.current = id;
  state.lastSeq = 0;
  state.calls.clear();
  state.changed.clear();
  state.asks.clear();
  setRunning(false);
  $("log").replaceChildren();
  renderChanges();
  renderApprovals();
  const session = currentSession();
  const project = projectById(session ? session.project : state.projectId);
  text("title", session && project ? `${project.name} // session ${short(session.id)}` : id);
  renderSessions();
  loadScratch();
  connect();
}

function connect() {
  const id = state.current;
  setStatus("linking…");
  state.link = backend.connect(id, state.lastSeq, {
    onopen() {
      state.online = true;
      setStatus("link established", "live");
      $("input").disabled = false;
      updateControls();
    },
    onevent(event) {
      if (event.type === "rejected") {
        addEntry("error", "rejected", event.reason);
        return;
      }
      if (event.seq <= state.lastSeq) return;
      state.lastSeq = event.seq;
      render(event);
    },
    onclose() {
      if (state.current !== id) return;
      state.online = false;
      setStatus("link lost · retrying", "down");
      $("input").disabled = true;
      updateControls();
      // Reconnect and replay from the last event seen. The session itself
      // kept running on the host the whole time.
      state.retry = setTimeout(async () => {
        await refreshSessions();
        if (state.current === id && !$("app").hidden) connect();
      }, 2000);
    },
  });
}

function setRunning(running) {
  state.running = running;
  updateControls();
  renderSessions();
}

function updateControls() {
  $("send").disabled = !state.online || state.running;
  $("stop").hidden = !state.running;
}

function addEntry(kind, who, body) {
  const log = $("log");
  const pinned = log.scrollHeight - log.scrollTop - log.clientHeight < 60;
  const entry = document.createElement("div");
  entry.className = `entry ${kind}`;
  entry.append(
    Object.assign(document.createElement("div"), { className: "who", textContent: who }),
    Object.assign(document.createElement("pre"), { textContent: body }),
  );
  log.append(entry);
  if (pinned) log.scrollTop = log.scrollHeight;
  return entry;
}

// Show a tool's input the way a person would read it: file content as
// text, not as an escaped JSON string.
function describeInput(input, brief) {
  if (input && typeof input.content === "string" && typeof input.path === "string") {
    const size = `${input.content.length} bytes`;
    return brief ? `${input.path} (${size})` : `${input.path} · ${size}\n${"─".repeat(24)}\n${input.content}`;
  }
  if (input && typeof input.content === "string" && typeof input.name === "string") {
    return brief ? `${input.name} (${input.kind || "artifact"})` : `${input.name} · ${input.kind || ""}\n${"─".repeat(24)}\n${input.content}`;
  }
  if (input && typeof input.path === "string" && Object.keys(input).length === 1) return input.path;
  return JSON.stringify(input, null, 2);
}

function send(message) {
  if (state.link) state.link.send(message);
}

function render(event) {
  switch (event.type) {
    case "user_message":
      setRunning(true);
      addEntry("user", "operator", event.text);
      break;
    case "assistant_message":
      addEntry("assistant", "agent", event.text);
      break;
    case "tool_call": {
      const path = event.input && typeof event.input.path === "string" ? event.input.path : null;
      const target = event.tool === "promote_scratch" && event.input ? event.input.to : path;
      state.calls.set(event.call_id, { tool: event.tool, path: target });
      addEntry("tool", `exec // ${event.tool}`, describeInput(event.input));
      break;
    }
    case "permission_requested": {
      const entry = addEntry("ask", "authorisation required", `${event.tool} → ${describeInput(event.input, true)}`);
      const actions = document.createElement("div");
      actions.className = "actions";
      for (const [name, allow] of [["Allow", true], ["Deny", false]]) {
        const button = Object.assign(document.createElement("button"), { type: "button", textContent: name });
        if (!allow) button.className = "danger";
        button.addEventListener("click", () => send({ type: "permission_decision", call_id: event.call_id, allow }));
        actions.append(button);
      }
      entry.append(actions);
      entry.scrollIntoView({ block: "end" });
      state.asks.set(event.call_id, { entry, tool: event.tool });
      renderApprovals();
      break;
    }
    case "permission_resolved": {
      const ask = state.asks.get(event.call_id);
      state.asks.delete(event.call_id);
      renderApprovals();
      if (!ask) break;
      ask.entry.querySelector(".actions")?.remove();
      ask.entry.classList.add(event.allowed ? "allowed" : "denied");
      ask.entry.append(
        Object.assign(document.createElement("div"), {
          className: "verdict",
          textContent: event.allowed ? "allowed" : `denied (${event.by})`,
        }),
      );
      break;
    }
    case "tool_result": {
      const call = state.calls.get(event.call_id) || {};
      state.calls.delete(event.call_id);
      if (event.ok) afterTool(call);
      addEntry(event.ok ? "result" : "result failed", event.ok ? "result" : "failed", event.output);
      break;
    }
    case "error":
      addEntry("error", "error", event.message);
      break;
    case "turn_finished":
      setRunning(false);
      break;
  }
}

// Refresh the panel a finished tool changed.
function afterTool(call) {
  if ((call.tool === "write_file" || call.tool === "promote_scratch") && call.path) {
    state.changed.add(call.path);
    renderChanges();
    loadFiles(state.cwd);
  }
  if (call.tool === "scratch_write") loadScratch();
  if (call.tool === "create_artifact") loadArtifacts();
  if (call.tool === "write_note") loadNotes();
}

// ---- approvals (status strip and inline) ----

function renderApprovals() {
  const count = state.asks.size;
  text("a-pending", String(count));
  const button = $("approvals-btn");
  button.hidden = count === 0;
  button.classList.toggle("pulse", count > 0);
  document.querySelector('#mobile-nav [data-view="chat"]').classList.toggle("badge", count > 0);
}

function reviewApproval() {
  const first = [...state.asks.values()][0];
  if (!first) return;
  setView("chat");
  first.entry.scrollIntoView({ block: "center" });
}

// ---- telemetry (status strip) ----

async function pollStatus() {
  if ($("app").hidden) return;
  try {
    const status = await backend.status();
    HUD.renderHost(status.host);
    const agent = status.agent;
    text("a-mode", agent.mode);
    text("a-running", String(agent.running));
    text("a-clients", String(agent.clients));
  } catch {
    /* next poll will try again */
  }
  setTimeout(pollStatus, 2000);
}

// ---- right column: tabs ----

function setTab(name) {
  state.tab = name;
  for (const button of document.querySelectorAll("#tabs [role='tab']")) {
    button.setAttribute("aria-selected", String(button.dataset.tab === name));
  }
  for (const pane of document.querySelectorAll(".pane")) {
    pane.hidden = pane.id !== `pane-${name}`;
  }
  if (name === "notes") loadNotes();
  if (name === "scratch") loadScratch();
  if (name === "artifacts") loadArtifacts();
}

// ---- files: the project's workspace ----

async function loadFiles(path, workspaceName) {
  const workspace = workspaceName || currentProject()?.workspace;
  if (!workspace) {
    $("grid").replaceChildren();
    $("crumbs").replaceChildren();
    text("fs-root", "");
    return;
  }
  state.cwd = path;
  text("fs-root", workspace);
  let entries = [];
  try {
    entries = await backend.files(workspace, path);
  } catch {
    return;
  }
  if (state.cwd !== path || currentProject()?.workspace !== workspace) return;

  const parts = path === "." ? [] : path.split("/");
  const crumbs = [["/", "."], ...parts.map((p, i) => [p, parts.slice(0, i + 1).join("/")])];
  $("crumbs").replaceChildren(
    ...crumbs.flatMap(([name, target], i) => {
      const crumb = Object.assign(document.createElement("button"), { type: "button", textContent: name });
      crumb.addEventListener("click", () => loadFiles(target, workspace));
      return i > 1 ? ["/", crumb] : [crumb];
    }),
  );

  const tile = (name, dir, onclick, changed) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `${dir ? "dir" : "file"}${changed ? " changed" : ""}`;
    button.title = name;
    button.append(Object.assign(document.createElement("i"), { className: "ico" }), Object.assign(document.createElement("span"), { textContent: name }));
    button.addEventListener("click", onclick);
    return button;
  };
  const join = (name) => (path === "." ? name : `${path}/${name}`);
  const tiles = entries.map((e) =>
    tile(e.name, e.dir, () => (e.dir ? loadFiles(join(e.name), workspace) : openWorkspaceFile(join(e.name), workspace)), state.changed.has(join(e.name))),
  );
  if (parts.length) tiles.unshift(tile("..", true, () => loadFiles(parts.slice(0, -1).join("/") || ".", workspace)));
  $("grid").replaceChildren(...tiles);
}

async function openWorkspaceFile(path, workspace) {
  const body = await backend.readFile(workspace, path).catch(() => "Cannot open this file.");
  openViewer(`${workspace} // ${path}`, { text: body, markdown: path.endsWith(".md") });
}

function renderChanges() {
  text("change-count", String(state.changed.size));
  $("changelist").replaceChildren(
    ...[...state.changed].map((path) => {
      const item = Object.assign(document.createElement("li"), { textContent: path, tabIndex: 0 });
      item.addEventListener("click", () => openWorkspaceFile(path, currentProject()?.workspace));
      return item;
    }),
  );
}

// ---- notes: the Obsidian vault ----

async function loadNotes() {
  const p = currentProject();
  const hasVault = Boolean(p && p.vault);
  $("note-empty").hidden = hasVault;
  $("note-search").hidden = !hasVault;
  if (!hasVault) {
    state.notes = [];
    $("note-list").replaceChildren();
    text("note-count", "");
    return;
  }
  try {
    state.notes = await backend.notes(p.id);
  } catch {
    state.notes = [];
  }
  text("note-count", String(state.notes.length));
  renderNoteList(state.notes.map((n) => ({ path: n.path, title: n.title, folder: n.path.includes("/") ? n.path.slice(0, n.path.lastIndexOf("/")) : "" })));
}

function renderNoteList(items, hits) {
  $("note-list").replaceChildren(
    ...items.map((item) => {
      const li = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = item.title;
      button.title = item.path;
      const meta = hits ? `${item.folder || "/"}:${item.line}` : item.folder;
      if (meta) button.append(Object.assign(document.createElement("small"), { textContent: meta }));
      button.addEventListener("click", () => openNote(item.path));
      li.append(button);
      return li;
    }),
  );
}

async function searchNotes(event) {
  event.preventDefault();
  const q = $("note-query").value.trim();
  const p = currentProject();
  if (!p || !p.vault) return;
  if (!q) {
    renderNoteList(state.notes.map((n) => ({ path: n.path, title: n.title, folder: n.path.includes("/") ? n.path.slice(0, n.path.lastIndexOf("/")) : "" })));
    return;
  }
  try {
    const hits = await backend.searchNotes(p.id, q);
    renderNoteList(
      hits.map((h) => ({ path: h.path, title: h.snippet || h.path, folder: h.path.split("/").slice(-1)[0].replace(/\.md$/, ""), line: h.line })),
      true,
    );
  } catch (e) {
    addEntry("error", "search", e.message);
  }
}

// Find the note a [[link]] names: a path if it has a folder, else a file name.
function resolveNote(target) {
  const t = target.replace(/\.md$/i, "").toLowerCase();
  const hit = state.notes.find((n) => {
    const p = n.path.replace(/\.md$/i, "").toLowerCase();
    return t.includes("/") ? p === t : p.split("/").pop() === t;
  });
  return hit ? hit.path : null;
}

async function openNote(path) {
  const p = currentProject();
  if (!p) return;
  if (!state.notes.length) await loadNotes();
  let body = "";
  try {
    body = await backend.note(p.id, path);
  } catch (e) {
    body = `Cannot open this note: ${e.message}`;
  }
  let links = [];
  try {
    links = await backend.backlinks(p.id, path);
  } catch {
    links = [];
  }
  const linkBlock = links.length
    ? (() => {
        const box = document.createElement("div");
        box.append(Object.assign(document.createElement("h3"), { textContent: "Linked from" }));
        for (const from of links) {
          const b = Object.assign(document.createElement("button"), { type: "button", textContent: from });
          b.addEventListener("click", () => openNote(from));
          box.append(b);
        }
        return box;
      })()
    : null;
  openViewer(`${p.name} // ${path}`, {
    markdown: true,
    text: body,
    link: (target, label) => {
      const resolved = resolveNote(target);
      const span = Object.assign(document.createElement("span"), { className: "wikilink", textContent: label });
      if (!resolved) {
        span.classList.add("unresolved");
        span.title = "No note with this name";
        return span;
      }
      span.addEventListener("click", () => openNote(resolved));
      return span;
    },
    links: linkBlock,
  });
}

// ---- scratch: this session's temporary files, and the archive ----

async function loadScratch() {
  const id = state.current;
  const path = state.scratchPath;
  const parts = path === "." ? [] : path.split("/");
  const crumbs = [["/", "."], ...parts.map((p, i) => [p, parts.slice(0, i + 1).join("/")])];
  $("scratch-crumbs").replaceChildren(
    ...crumbs.flatMap(([name, target], i) => {
      const crumb = Object.assign(document.createElement("button"), { type: "button", textContent: name });
      crumb.addEventListener("click", () => {
        state.scratchPath = target;
        loadScratch();
      });
      return i > 1 ? ["/", crumb] : [crumb];
    }),
  );
  let entries = [];
  if (id) {
    try {
      entries = await backend.scratchList(id, path);
    } catch {
      entries = [];
    }
  }
  if (id !== state.current || path !== state.scratchPath) return;
  $("scratch-list").replaceChildren(
    ...entries.map((e) => {
      const li = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      const rel = path === "." ? e.name : `${path}/${e.name}`;
      button.textContent = e.dir ? `${e.name}/` : e.name;
      if (!e.dir) button.append(Object.assign(document.createElement("small"), { textContent: `${e.size} B` }));
      button.addEventListener("click", () => {
        if (e.dir) {
          state.scratchPath = rel;
          loadScratch();
        } else {
          openScratchFile(rel);
        }
      });
      li.append(button);
      return li;
    }),
  );
  if (id) {
    const items = await backend.archives().catch(() => []);
    if (id !== state.current) return;
    renderArchives(items);
  } else {
    renderArchives([]);
  }
}

function renderArchives(items) {
  $("archive-list").replaceChildren(
    ...items.map((a) => {
      const li = document.createElement("li");
      const label = Object.assign(document.createElement("button"), {
        type: "button",
        textContent: `session ${short(a.session)}`,
        title: a.name,
      });
      label.append(Object.assign(document.createElement("small"), { textContent: `${new Date(a.archived_ms).toLocaleDateString()} · ${a.files} files` }));
      const restore = Object.assign(document.createElement("button"), { type: "button", className: "x", textContent: "Restore" });
      restore.addEventListener("click", async () => {
        try {
          await backend.restoreArchive(a.name);
          loadScratch();
        } catch (e) {
          addEntry("error", "restore", e.message);
        }
      });
      li.append(label, restore);
      return li;
    }),
  );
}

function openScratchFile(rel) {
  const id = state.current;
  const input = Object.assign(document.createElement("input"), { placeholder: "project path, e.g. docs/draft.md", "aria-label": "Project path" });
  const promote = Object.assign(document.createElement("button"), { type: "button", textContent: "Promote to project" });
  promote.addEventListener("click", () => {
    const to = input.value.trim();
    if (!to || !state.online || state.running) return;
    send({ type: "user_message", text: `/promote ${rel} ${to}` });
    closeViewer();
  });
  const note = Object.assign(document.createElement("span"), { className: "muted", textContent: "Promoting asks for approval, like any write." });
  backend.scratchRead(id, rel).then(
    (body) => openViewer(`scratch // ${rel}`, { text: body, markdown: rel.endsWith(".md"), actions: [input, promote, note] }),
    () => openViewer(`scratch // ${rel}`, { text: "Cannot open this file.", actions: [] }),
  );
}

// ---- artifacts ----

async function loadArtifacts() {
  const p = currentProject();
  if (!p) return;
  try {
    state.artifacts = await backend.artifacts(p.id);
  } catch {
    state.artifacts = [];
  }
  text("artifact-count", String(state.artifacts.length));
  $("artifact-list").replaceChildren(
    ...state.artifacts.map((a) => {
      const li = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      const latest = a.versions[a.versions.length - 1];
      button.textContent = a.name;
      button.append(Object.assign(document.createElement("small"), { textContent: `${a.kind} · v${latest ? latest.version : 0}` }));
      button.addEventListener("click", () => openArtifact(a.name, latest ? latest.version : null));
      li.append(button);
      return li;
    }),
  );
}

async function openArtifact(name, version) {
  const p = currentProject();
  if (!p) return;
  let data;
  try {
    data = await backend.artifact(p.id, name, version);
  } catch (e) {
    openViewer(name, { text: e.message });
    return;
  }
  const meta = data.meta;
  const actions = [];
  if (meta.versions.length > 1) {
    const select = document.createElement("select");
    select.setAttribute("aria-label", "Version");
    for (const v of meta.versions) {
      select.append(Object.assign(document.createElement("option"), { value: String(v.version), textContent: `v${v.version}`, selected: v.version === data.version }));
    }
    select.addEventListener("change", () => openArtifact(name, Number(select.value)));
    actions.push(select);
  }
  const download = Object.assign(document.createElement("button"), { type: "button", textContent: "Download" });
  download.addEventListener("click", () => {
    const ext = { html: "html", svg: "svg", markdown: "md", code: "txt" }[meta.kind] || "txt";
    const url = URL.createObjectURL(new Blob([data.text], { type: "text/plain" }));
    const a = Object.assign(document.createElement("a"), { href: url, download: `${name}-v${data.version}.${ext}` });
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  actions.push(download);
  const title = `${name} · ${meta.kind} · v${data.version}`;
  if (meta.kind === "html" || meta.kind === "svg") {
    // Rendered in a sandboxed frame. The daemon sends it with a policy that
    // allows no network, so it cannot reach the daemon or the internet.
    const frame = backend.demo
      ? { srcdoc: data.text }
      : { src: `${state.frameBase}/${p.id}/${encodeURIComponent(name)}/${data.version}` };
    openViewer(title, { frame, actions });
  } else {
    openViewer(title, { text: data.text, markdown: meta.kind === "markdown", actions });
  }
}

// ---- viewer: one dialog for files, notes, scratch and artifacts ----

function openViewer(title, { text: body, markdown = false, markdownOpts, link, links, frame, actions = [] } = {}) {
  text("viewer-title", title);
  const actionBox = $("viewer-actions");
  actionBox.replaceChildren(...actions);
  const pre = $("viewer-body");
  const md = $("viewer-md");
  const iframe = $("viewer-frame");
  const linkBox = $("viewer-links");
  pre.hidden = true;
  md.hidden = true;
  iframe.hidden = true;
  iframe.removeAttribute("srcdoc");
  iframe.src = "about:blank";
  linkBox.hidden = true;
  linkBox.replaceChildren();
  if (frame) {
    iframe.hidden = false;
    if (frame.srcdoc !== undefined) iframe.srcdoc = frame.srcdoc;
    else iframe.src = frame.src;
  } else if (markdown) {
    md.hidden = false;
    md.replaceChildren(window.Markdown.render(body || "", { link: link || undefined, ...(markdownOpts || {}) }));
  } else {
    pre.hidden = false;
    pre.textContent = body || "";
  }
  if (links) {
    linkBox.hidden = false;
    linkBox.replaceChildren(links);
  }
  $("viewer").hidden = false;
  $("viewer-close").focus();
}

function closeViewer() {
  $("viewer").hidden = true;
  $("viewer-frame").src = "about:blank";
  $("viewer-frame").removeAttribute("srcdoc");
}

// ---- projects: create ----

async function createProject(event) {
  event.preventDefault();
  const name = $("np-name").value.trim();
  const vault = $("np-vault").value.trim();
  const body = {
    name,
    workspace: $("np-workspace").value,
    vault: vault || undefined,
    vault_mode: $("np-mode").value,
  };
  try {
    const project = await backend.createProject(body);
    state.projects = [...state.projects, project];
    $("new-project").reset();
    $("new-project").hidden = true;
    $("new-project-toggle").setAttribute("aria-expanded", "false");
    $("np-error").hidden = true;
    renderProjects();
    await selectProject(project.id);
  } catch (e) {
    $("np-error").hidden = false;
    text("np-error", e.message);
  }
}

// ---- chrome ----

function setView(view) {
  $("app").dataset.view = view;
  for (const button of document.querySelectorAll("#mobile-nav button")) {
    button.classList.toggle("active", button.dataset.view === view);
  }
}

for (const button of document.querySelectorAll("#mobile-nav button")) {
  button.addEventListener("click", () => setView(button.dataset.view));
}
for (const button of document.querySelectorAll("#tabs [role='tab']")) {
  button.addEventListener("click", () => setTab(button.dataset.tab));
}

$("toggle-left").addEventListener("click", () => $("app").classList.toggle("no-left"));
$("toggle-right").addEventListener("click", () => $("app").classList.toggle("no-right"));
$("approvals-btn").addEventListener("click", reviewApproval);
$("viewer-close").addEventListener("click", closeViewer);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeViewer();
});

$("login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const ok = await backend.login($("token").value);
  $("login-error").hidden = ok;
  if (ok) {
    $("token").value = "";
    boot();
  }
});

$("logout").addEventListener("click", async () => {
  await backend.logout();
  state.current = null;
  if (backend.demo) location.reload();
  else showLogin();
});

$("new-project-toggle").addEventListener("click", () => {
  const form = $("new-project");
  form.hidden = !form.hidden;
  $("new-project-toggle").setAttribute("aria-expanded", String(!form.hidden));
  if (!form.hidden) $("np-name").focus();
});
$("new-project").addEventListener("submit", createProject);

$("p-mode").addEventListener("change", () => changeProject({ vault_mode: $("p-mode").value }));
$("p-vault-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const path = $("p-vault-path").value.trim();
  if (path) changeProject({ vault: path });
});
$("p-vault-detach").addEventListener("click", () => changeProject({ vault: null }));

$("new-session").addEventListener("click", newSession);
$("note-search").addEventListener("submit", searchNotes);

$("composer").addEventListener("submit", (e) => {
  e.preventDefault();
  const message = $("input").value.trim();
  if (!message) return;
  if (message === "/new") {
    $("input").value = "";
    newSession();
    return;
  }
  if (!state.online || state.running) return;
  send({ type: "user_message", text: message });
  $("input").value = "";
});

$("input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    $("composer").requestSubmit();
  }
});

$("stop").addEventListener("click", () => send({ type: "interrupt" }));

HUD.initTheme();
HUD.clock();
boot();
