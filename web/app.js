// Unchained web client. No build step: this file is embedded in the daemon
// binary and served as-is. All server-supplied text is inserted with
// textContent, never as HTML.
"use strict";

const $ = (id) => document.getElementById(id);
const state = { sessions: [], current: null, socket: null, lastSeq: 0, running: false, retry: null };

async function api(path, options = {}) {
  const response = await fetch("/api" + path, {
    credentials: "same-origin",
    headers: options.body ? { "content-type": "application/json" } : {},
    ...options,
  });
  if (response.status === 401) {
    showLogin();
    throw new Error("unauthorized");
  }
  return response;
}

function showLogin() {
  closeSocket();
  $("app").hidden = true;
  $("login").hidden = false;
  $("token").focus();
}

async function boot() {
  let workspaces;
  try {
    workspaces = await (await api("/workspaces")).json();
  } catch {
    return;
  }
  $("login").hidden = true;
  $("app").hidden = false;
  $("workspace").replaceChildren(
    ...workspaces.map((w) => {
      const option = document.createElement("option");
      option.value = w.name;
      option.textContent = w.name;
      option.title = w.root;
      return option;
    }),
  );
  await refreshSessions();
  if (!state.current && state.sessions.length) openSession(state.sessions[0].id);
}

async function refreshSessions() {
  try {
    state.sessions = await (await api("/sessions")).json();
  } catch {
    return;
  }
  renderSessions();
}

function label(session) {
  const when = new Date(session.created_ms).toLocaleString([], {
    month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
  });
  return `${session.workspace} · ${when}`;
}

function renderSessions() {
  $("sessions").replaceChildren(
    ...state.sessions.map((s) => {
      const item = document.createElement("li");
      item.textContent = label(s);
      item.classList.toggle("active", s.id === state.current);
      const running = s.id === state.current ? state.running : s.running;
      item.classList.toggle("running", running);
      item.addEventListener("click", () => openSession(s.id));
      return item;
    }),
  );
}

function closeSocket() {
  clearTimeout(state.retry);
  if (state.socket) {
    state.socket.onclose = null;
    state.socket.close();
    state.socket = null;
  }
}

function openSession(id) {
  closeSocket();
  state.current = id;
  state.lastSeq = 0;
  setRunning(false);
  $("log").replaceChildren();
  const session = state.sessions.find((s) => s.id === id);
  $("title").textContent = session ? label(session) : id;
  renderSessions();
  connect();
}

function connect() {
  const id = state.current;
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const socket = new WebSocket(
    `${scheme}://${location.host}/api/sessions/${encodeURIComponent(id)}/ws?after=${state.lastSeq}`,
  );
  state.socket = socket;
  $("status").textContent = "connecting…";
  socket.onopen = () => {
    $("status").textContent = "connected";
    $("input").disabled = false;
    updateControls();
  };
  socket.onmessage = (message) => {
    const event = JSON.parse(message.data);
    if (event.type === "rejected") {
      addEntry("error", "rejected", event.reason);
      return;
    }
    if (event.seq <= state.lastSeq) return;
    state.lastSeq = event.seq;
    render(event);
  };
  socket.onclose = () => {
    if (state.current !== id) return;
    $("status").textContent = "disconnected, retrying…";
    $("input").disabled = true;
    updateControls();
    // Reconnect and replay from the last event we saw. The session itself
    // kept running on the host the whole time.
    state.retry = setTimeout(async () => {
      await refreshSessions();
      if (state.current === id && !$("app").hidden) connect();
    }, 2000);
  };
}

function setRunning(running) {
  state.running = running;
  updateControls();
  renderSessions();
}

function updateControls() {
  const open = state.socket && state.socket.readyState === WebSocket.OPEN;
  $("send").disabled = !open || state.running;
  $("stop").hidden = !state.running;
}

function addEntry(kind, who, text) {
  const log = $("log");
  const pinned = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
  const entry = document.createElement("div");
  entry.className = `entry ${kind}`;
  const header = document.createElement("div");
  header.className = "who";
  header.textContent = who;
  const body = document.createElement("pre");
  body.textContent = text;
  entry.append(header, body);
  log.append(entry);
  if (pinned) log.scrollTop = log.scrollHeight;
  return entry;
}

function send(message) {
  if (state.socket && state.socket.readyState === WebSocket.OPEN) {
    state.socket.send(JSON.stringify(message));
  }
}

function render(event) {
  switch (event.type) {
    case "user_message":
      setRunning(true);
      addEntry("user", "you", event.text);
      break;
    case "assistant_message":
      addEntry("assistant", "agent", event.text);
      break;
    case "tool_call":
      addEntry("tool", `tool · ${event.tool}`, JSON.stringify(event.input, null, 2));
      break;
    case "permission_requested": {
      const entry = addEntry(
        "ask",
        "approval needed",
        `${event.tool} ${JSON.stringify(event.input, null, 2)}`,
      );
      entry.dataset.callId = event.call_id;
      const actions = document.createElement("div");
      actions.className = "actions";
      for (const [text, allow] of [["Allow", true], ["Deny", false]]) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = text;
        if (!allow) button.className = "danger";
        button.addEventListener("click", () =>
          send({ type: "permission_decision", call_id: event.call_id, allow }),
        );
        actions.append(button);
      }
      entry.append(actions);
      break;
    }
    case "permission_resolved": {
      const entry = [...document.querySelectorAll(".entry.ask")].find(
        (e) => e.dataset.callId === event.call_id,
      );
      if (!entry) break;
      entry.querySelector(".actions")?.remove();
      entry.classList.add(event.allowed ? "allowed" : "denied");
      const verdict = document.createElement("div");
      verdict.className = "verdict";
      verdict.textContent = event.allowed ? "allowed" : `denied (${event.by})`;
      entry.append(verdict);
      break;
    }
    case "tool_result":
      addEntry(event.ok ? "result" : "result failed", event.ok ? "result" : "failed", event.output);
      break;
    case "error":
      addEntry("error", "error", event.message);
      break;
    case "turn_finished":
      setRunning(false);
      break;
  }
}

$("login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const response = await fetch("/api/login", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token: $("token").value }),
  });
  $("login-error").hidden = response.ok;
  if (response.ok) {
    $("token").value = "";
    boot();
  }
});

$("logout").addEventListener("click", async () => {
  await fetch("/api/logout", { method: "POST", credentials: "same-origin" });
  state.current = null;
  showLogin();
});

$("new-session").addEventListener("submit", async (e) => {
  e.preventDefault();
  const response = await api("/sessions", {
    method: "POST",
    body: JSON.stringify({ workspace: $("workspace").value }),
  });
  if (!response.ok) return;
  const session = await response.json();
  await refreshSessions();
  openSession(session.id);
});

$("composer").addEventListener("submit", (e) => {
  e.preventDefault();
  const text = $("input").value.trim();
  if (!text || $("send").disabled) return;
  send({ type: "user_message", text });
  $("input").value = "";
});

$("input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    $("composer").requestSubmit();
  }
});

$("stop").addEventListener("click", () => send({ type: "interrupt" }));

boot();
