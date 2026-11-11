// Host console: a concept for what the machine running the daemon shows on
// its own display. It runs entirely on the simulation in demo.js; the
// daemon does not serve this page yet.
"use strict";

(() => {
  const { $, text, bar, spark } = HUD;
  const backend = window.UnchainedDemo;
  const links = new Map(); // session id -> link
  const asks = new Map(); // call_id -> { session, tool, path }
  const stamp = () => new Date().toLocaleTimeString([], { hour12: false });

  function feed(id, parts, limit = 40) {
    const box = $(id);
    const line = document.createElement("div");
    line.append(Object.assign(document.createElement("time"), { textContent: stamp() }));
    for (const [cls, value] of parts) {
      line.append(Object.assign(document.createElement("span"), { className: cls, textContent: value }));
    }
    box.append(line);
    while (box.children.length > limit) box.firstChild.remove();
  }

  const clip = (s, n = 90) => (s.length > n ? s.slice(0, n) + "…" : s).replace(/\s+/g, " ");

  function describe(event) {
    switch (event.type) {
      case "user_message": return `operator: ${clip(event.text)}`;
      case "assistant_message": return `agent: ${clip(event.text)}`;
      case "tool_call": return `exec ${event.tool} ${clip(JSON.stringify(event.input), 60)}`;
      case "permission_requested": return `WAITING for approval: ${event.tool} ${event.input.path ?? ""}`;
      case "permission_resolved": return event.allowed ? "approved" : `denied (${event.by})`;
      case "tool_result": return event.ok ? "tool ok" : `tool failed: ${clip(event.output, 60)}`;
      case "error": return `error: ${event.message}`;
      case "turn_finished": return "turn finished";
      default: return event.type;
    }
  }

  function watch(session) {
    if (links.has(session.id)) return;
    const short = session.id.slice(0, 4);
    const link = backend.connect(session.id, 0, {
      onopen() {},
      onclose() {},
      onevent(event) {
        feed("activity", [["s", short], ["", describe(event)]]);
        if (event.type === "permission_requested") {
          asks.set(event.call_id, { link, short, tool: event.tool, path: event.input.path ?? "" });
        } else if (event.type === "permission_resolved") {
          asks.delete(event.call_id);
        }
        renderAsks();
      },
    });
    links.set(session.id, link);
  }

  function renderAsks() {
    text("ask-count", String(asks.size));
    $("ask-count").classList.toggle("alert", asks.size > 0);
    $("asks").replaceChildren(
      ...[...asks.entries()].map(([call_id, ask]) => {
        const item = document.createElement("li");
        item.append(Object.assign(document.createElement("span"), { textContent: `${ask.short} · ${ask.tool}` }));
        for (const [name, allow] of [["Allow", true], ["Deny", false]]) {
          const button = Object.assign(document.createElement("button"), { type: "button", textContent: name });
          if (!allow) button.className = "danger";
          button.addEventListener("click", () => ask.link.send({ type: "permission_decision", call_id, allow }));
          item.append(button);
        }
        item.append(Object.assign(document.createElement("small"), { textContent: ask.path }));
        return item;
      }),
    );
  }

  async function tick() {
    const [status, sessions] = await Promise.all([backend.status(), backend.sessions()]);
    HUD.renderHost(status.host);
    const { model } = status.agent;
    text("m-name", model.name);
    text("m-tps", `${model.tps.toFixed(1)} tok/s`);
    spark("m-tps-spark", model.tps, 80);
    text("m-queue", String(Math.max(0, status.agent.running - 1)));
    text("m-ctx", `${Math.round(model.ctx_used / 1000)}k / ${Math.round(model.ctx_max / 1000)}k`);
    bar("m-ctx-bar", model.ctx_used / model.ctx_max);

    text("session-count", `${status.agent.running} running`);
    $("session-rows").replaceChildren(
      ...sessions.map((s) => {
        watch(s);
        const waiting = [...asks.values()].some((a) => a.short === s.id.slice(0, 4));
        const stateName = waiting ? "waiting" : s.running ? "running" : "idle";
        const row = document.createElement("tr");
        const cells = [s.id.slice(0, 8), s.workspace, stateName, String(s.last_seq), new Date(s.created_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })];
        cells.forEach((value, i) => {
          const cell = Object.assign(document.createElement("td"), { textContent: value });
          if (i === 2) cell.className = `state-${stateName}`;
          row.append(cell);
        });
        return row;
      }),
    );
  }

  const LOG = [
    "telemetry sample ok",
    "client heartbeat: laptop",
    "client heartbeat: phone",
    "session log flushed",
    "model server healthy · 127.0.0.1:8080",
    "checkpoint ref updated",
  ];

  $("stop-all").addEventListener("click", () => {
    for (const link of links.values()) link.send({ type: "interrupt" });
    feed("daemon-log", [["alert", "STOP ALL issued from host console"]], 12);
  });

  HUD.initTheme();
  HUD.clock();
  feed("daemon-log", [["", "unchained daemon started · mode ask · 2 workspaces"]], 12);
  feed("daemon-log", [["", "listening on 100.64.0.7:7878 (private overlay)"]], 12);
  tick();
  setInterval(tick, 2000);
  setInterval(() => feed("daemon-log", [["", LOG[Math.floor(Math.random() * LOG.length)]]], 12), 3500);
})();
