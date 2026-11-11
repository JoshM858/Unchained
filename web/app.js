// Unchained web client. No build step: these files are embedded in the
// daemon binary and served as-is. All server-supplied text is inserted with
// textContent, never as HTML.
//
// The client talks to a "backend" object. The real one below uses the
// daemon's HTTP and WebSocket API; `?demo` swaps in the in-browser
// simulation from demo.js so the interface can be shown with no daemon.
"use strict";

const { $, text, bar, spark } = HUD;

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
    return (await this.request("/workspaces")).json();
  },
  async sessions() {
    return (await this.request("/sessions")).json();
  },
  async createSession(workspace) {
    const response = await this.request("/sessions", { method: "POST", body: JSON.stringify({ workspace }) });
    return response.ok ? response.json() : null;
  },
  async status() {
    return (await this.request("/status")).json();
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
  sessions: [],
  current: null,
  link: null,
  online: false,
  lastSeq: 0,
  running: false,
  retry: null,
  cwd: ".",
  writes: new Map(), // call_id -> path of an in-flight write
  changed: new Set(),
  asks: new Map(), // call_id -> entry element, while unanswered
};

function currentWorkspace() {
  const session = state.sessions.find((s) => s.id === state.current);
  return session ? session.workspace : $("workspace").value;
}

function showLogin() {
  closeLink();
  $("app").hidden = true;
  $("login").hidden = false;
  $("token").focus();
}

async function boot() {
  let workspaces;
  try {
    workspaces = await backend.workspaces();
  } catch {
    return;
  }
  $("login").hidden = true;
  $("app").hidden = false;
  $("workspace").replaceChildren(
    ...workspaces.map((w) => Object.assign(document.createElement("option"), { value: w.name, textContent: w.name, title: w.root })),
  );
  await refreshSessions();
  if (!state.current && state.sessions.length) openSession(state.sessions[0].id);
  else loadFiles(".");
  pollStatus();
}

async function refreshSessions() {
  try {
    state.sessions = await backend.sessions();
  } catch {
    return;
  }
  renderTabs();
}

function label(session) {
  return `${session.workspace} · ${session.id.slice(0, 4)}`;
}

function renderTabs() {
  $("tablist").replaceChildren(
    ...state.sessions.map((s) => {
      const tab = document.createElement("button");
      tab.type = "button";
      tab.role = "tab";
      tab.textContent = label(s);
      tab.title = new Date(s.created_ms).toLocaleString();
      const active = s.id === state.current;
      tab.setAttribute("aria-selected", String(active));
      tab.classList.toggle("active", active);
      tab.classList.toggle("running", active ? state.running : s.running);
      tab.addEventListener("click", () => openSession(s.id));
      return tab;
    }),
  );
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

function openSession(id) {
  closeLink();
  state.current = id;
  state.lastSeq = 0;
  state.writes.clear();
  state.changed.clear();
  state.asks.clear();
  setRunning(false);
  $("log").replaceChildren();
  renderChanges();
  renderApprovals();
  const session = state.sessions.find((s) => s.id === id);
  text("title", session ? `${session.workspace} // session ${session.id.slice(0, 8)}` : id);
  renderTabs();
  loadFiles(".");
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
  renderTabs();
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
    case "tool_call":
      if (event.tool === "write_file" && event.input && typeof event.input.path === "string") {
        state.writes.set(event.call_id, event.input.path);
      }
      addEntry("tool", `exec // ${event.tool}`, describeInput(event.input));
      break;
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
      const path = state.writes.get(event.call_id);
      state.writes.delete(event.call_id);
      if (path && event.ok) {
        state.changed.add(path);
        renderChanges();
        loadFiles(state.cwd);
      }
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

// ---- approvals (right panel) ----

function renderApprovals() {
  const count = state.asks.size;
  text("a-pending", String(count));
  $("a-pending").classList.toggle("alert", count > 0);
  $("a-pending").classList.toggle("pulse", count > 0);
  document.querySelector('#mobile-nav [data-view="chat"]').classList.toggle("badge", count > 0);
  const box = $("approvals");
  if (!count) {
    box.replaceChildren(Object.assign(document.createElement("span"), { className: "muted", textContent: "Nothing waiting." }));
    return;
  }
  box.replaceChildren(
    ...[...state.asks.values()].map(({ entry, tool }) => {
      const jump = Object.assign(document.createElement("button"), { type: "button", className: "danger", textContent: `Review ${tool}` });
      jump.addEventListener("click", () => {
        setView("chat");
        entry.scrollIntoView({ block: "center" });
      });
      return jump;
    }),
  );
}

// ---- telemetry ----

async function pollStatus() {
  if ($("app").hidden) return;
  try {
    const status = await backend.status();
    HUD.renderHost(status.host);
    const agent = status.agent;
    text("a-version", `v${agent.version}`);
    text("a-model", agent.model.name);
    text("a-mode", agent.mode);
    text("a-tps", agent.model.tps == null ? "—" : `${agent.model.tps.toFixed(1)} tok/s`);
    spark("a-tps-spark", agent.model.tps || 0, 80);
    const { ctx_used: used, ctx_max: max } = agent.model;
    text("a-ctx", max ? `${Math.round(used / 1000)}k / ${Math.round(max / 1000)}k` : "—");
    bar("a-ctx-bar", max ? used / max : 0);
    text("a-sessions", String(agent.sessions));
    text("a-running", String(agent.running));
    text("a-clients", String(agent.clients));
  } catch {
    /* next poll will try again */
  }
  setTimeout(pollStatus, 2000);
}

// ---- file browser ----

async function loadFiles(path) {
  const workspace = currentWorkspace();
  if (!workspace) return;
  state.cwd = path;
  text("fs-root", workspace);
  let entries = [];
  try {
    entries = await backend.files(workspace, path);
  } catch {
    return;
  }
  if (state.cwd !== path || currentWorkspace() !== workspace) return;

  const parts = path === "." ? [] : path.split("/");
  const crumbs = [["/", "."], ...parts.map((p, i) => [p, parts.slice(0, i + 1).join("/")])];
  $("crumbs").replaceChildren(
    ...crumbs.flatMap(([name, target], i) => {
      const crumb = Object.assign(document.createElement("button"), { type: "button", textContent: name });
      crumb.addEventListener("click", () => loadFiles(target));
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
    tile(e.name, e.dir, () => (e.dir ? loadFiles(join(e.name)) : openFile(join(e.name))), state.changed.has(join(e.name))),
  );
  if (parts.length) tiles.unshift(tile("..", true, () => loadFiles(parts.slice(0, -1).join("/") || ".")));
  $("grid").replaceChildren(...tiles);
}

async function openFile(path) {
  text("viewer-title", `${currentWorkspace()} // ${path}`);
  text("viewer-body", "Loading…");
  $("viewer").hidden = false;
  $("viewer-close").focus();
  try {
    text("viewer-body", await backend.readFile(currentWorkspace(), path));
  } catch {
    text("viewer-body", "Cannot open this file.");
  }
}

function renderChanges() {
  text("change-count", String(state.changed.size));
  $("changelist").replaceChildren(
    ...[...state.changed].map((path) => {
      const item = Object.assign(document.createElement("li"), { textContent: path, tabIndex: 0 });
      item.addEventListener("click", () => openFile(path));
      return item;
    }),
  );
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

$("focus").addEventListener("click", () => $("app").classList.toggle("focus"));
$("viewer-close").addEventListener("click", () => ($("viewer").hidden = true));
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") $("viewer").hidden = true;
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

$("workspace").addEventListener("change", () => {
  if (!state.current) loadFiles(".");
});

$("new-session").addEventListener("submit", async (e) => {
  e.preventDefault();
  const session = await backend.createSession($("workspace").value);
  if (!session) return;
  await refreshSessions();
  openSession(session.id);
});

$("composer").addEventListener("submit", (e) => {
  e.preventDefault();
  const message = $("input").value.trim();
  if (!message || $("send").disabled) return;
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
