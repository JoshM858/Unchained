// In-browser simulation of the daemon, for showing the interface without
// one (`index.html?demo`, and the GitHub Pages demo). It speaks the same
// event protocol as the real backend, so the client code path is identical.
// Nothing here touches a real machine or a real model.
"use strict";

window.UnchainedDemo = (() => {
  const GB = 1024 ** 3;
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const walk = (v, step, lo, hi) => Math.max(lo, Math.min(hi, v + (Math.random() - 0.5) * step));

  const LIMITER = `pub struct Limiter {
    window: Duration,
    max: u32,
    started: Instant,
    count: u32,
}

impl Limiter {
    pub fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.started) > self.window {
            self.started = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= self.max
    }
}
`;
  const LIMITER_FIXED = LIMITER.replace("> self.window", ">= self.window");
  const TEST = `#[test]
fn resets_exactly_at_window_boundary() {
    let t0 = Instant::now();
    let mut l = Limiter::new(Duration::from_secs(60), 2);
    assert!(l.allow(t0));
    assert!(l.allow(t0));
    assert!(l.allow(t0 + Duration::from_secs(60))); // fails
}
`;

  // workspace -> { path: content }
  const fs = {
    "atlas-api": {
      "README.md": "# atlas-api\n\nInternal API gateway.\n",
      "Cargo.toml": '[package]\nname = "atlas-api"\nversion = "0.4.2"\n',
      "src/main.rs": "mod limiter;\nmod routes;\n\nfn main() {\n    routes::serve();\n}\n",
      "src/limiter.rs": LIMITER,
      "src/routes.rs": "pub fn serve() {\n    // ...\n}\n",
      "tests/limiter_test.rs": TEST,
    },
    "homelab-infra": {
      "README.md": "# homelab-infra\n\nCompose files and backup scripts.\n",
      "compose.yml": "services:\n  llm:\n    image: local/llm-server\n",
      "scripts/backup.sh": "#!/bin/sh\nrsync -a /srv/data /mnt/backup\n",
    },
  };

  const sessions = new Map();
  let nextId = 0xa41f;

  function makeSession(workspace, ageMs) {
    const id = (nextId++).toString(16).padStart(4, "0") + "c9e27b10";
    const session = { id, workspace, created_ms: Date.now() - ageMs, events: [], subs: new Set(), pending: new Map(), running: false, stop: false };
    sessions.set(id, session);
    return session;
  }

  function emit(session, kind) {
    const event = { seq: session.events.length + 1, ts_ms: Date.now(), ...kind };
    session.events.push(event);
    for (const sub of session.subs) sub(event);
    return event;
  }

  const callId = () => Math.random().toString(16).slice(2, 10);

  async function runTool(session, tool, input) {
    const call_id = callId();
    emit(session, { type: "tool_call", call_id, tool, input });
    await wait(350);
    const files = fs[session.workspace];
    const fail = (output) => emit(session, { type: "tool_result", call_id, ok: false, output }) && false;
    const path = String(input.path ?? ".").replace(/^\.\//, "");
    if (path.startsWith("/") || path.split("/").includes("..")) return fail(`path escapes the workspace: ${path}`);

    if (tool === "write_file") {
      emit(session, { type: "permission_requested", call_id, tool, input });
      const allowed = await new Promise((resolve) => session.pending.set(call_id, resolve));
      session.pending.delete(call_id);
      emit(session, { type: "permission_resolved", call_id, allowed, by: session.stop ? "interrupt" : "client" });
      if (!allowed) return fail("permission denied");
      files[path] = input.content;
      emit(session, { type: "tool_result", call_id, ok: true, output: `wrote ${input.content.length} bytes to ${path}` });
      return true;
    }
    if (tool === "read_file") {
      if (!(path in files)) return fail(`${path}: No such file or directory`);
      emit(session, { type: "tool_result", call_id, ok: true, output: files[path] });
      return true;
    }
    const listing = list(session.workspace, path);
    if (!listing.length && path !== ".") return fail(`${path}: No such file or directory`);
    emit(session, { type: "tool_result", call_id, ok: true, output: listing.map((e) => e.name + (e.dir ? "/" : "")).join("\n") });
    return true;
  }

  function list(workspace, path) {
    const prefix = path === "." ? "" : path.replace(/\/$/, "") + "/";
    const seen = new Map();
    for (const [file, content] of Object.entries(fs[workspace] || {})) {
      if (!file.startsWith(prefix)) continue;
      const rest = file.slice(prefix.length);
      const [head, ...tail] = rest.split("/");
      if (tail.length) seen.set(head, { name: head, dir: true, size: 0 });
      else seen.set(head, { name: head, dir: false, size: content.length });
    }
    return [...seen.values()].sort((a, b) => b.dir - a.dir || a.name.localeCompare(b.name));
  }

  function finish(session) {
    session.running = false;
    session.stop = false;
    emit(session, { type: "turn_finished" });
  }

  async function say(session, message) {
    await wait(500);
    emit(session, { type: "assistant_message", text: message });
  }

  async function turn(session, message) {
    session.running = true;
    emit(session, { type: "user_message", text: message });
    const [cmd, ...rest] = message.trim().split(/\s+/);
    if (cmd === "/ls") await runTool(session, "list_dir", { path: rest[0] || "." });
    else if (cmd === "/read" && rest[0]) await runTool(session, "read_file", { path: rest[0] });
    else if (cmd === "/write" && rest[0]) await runTool(session, "write_file", { path: rest[0], content: rest.slice(1).join(" ") });
    else {
      await say(session, "This is a simulated agent running in your browser: no model and no machine are connected. It understands /ls [dir], /read <file> and /write <file> <text>. Writes wait for your approval, as they would on a real host.");
    }
    finish(session);
  }

  // Session 1 opens mid-task, paused on an approval.
  async function seed() {
    const a = makeSession("atlas-api", 42 * 60_000);
    a.running = true;
    emit(a, { type: "user_message", text: "The rate limiter test is failing on CI. Find out why and fix it." });
    emit(a, { type: "assistant_message", text: "I'll read the failing test and the limiter it exercises." });
    for (const path of ["tests/limiter_test.rs", "src/limiter.rs"]) {
      const call_id = callId();
      emit(a, { type: "tool_call", call_id, tool: "read_file", input: { path } });
      emit(a, { type: "tool_result", call_id, ok: true, output: fs["atlas-api"][path] });
    }
    emit(a, {
      type: "assistant_message",
      text: "Found it. The window check uses `>`, so a request arriving exactly 60s after the window opened is still counted in the old window and rejected. The test expects the window to reset at the boundary. Changing the comparison to `>=` fixes it.",
    });
    const call_id = callId();
    const input = { path: "src/limiter.rs", content: LIMITER_FIXED };
    emit(a, { type: "tool_call", call_id, tool: "write_file", input });
    emit(a, { type: "permission_requested", call_id, tool: "write_file", input });
    new Promise((resolve) => a.pending.set(call_id, resolve)).then(async (allowed) => {
      a.pending.delete(call_id);
      emit(a, { type: "permission_resolved", call_id, allowed, by: a.stop ? "interrupt" : "client" });
      if (allowed) {
        fs["atlas-api"]["src/limiter.rs"] = LIMITER_FIXED;
        emit(a, { type: "tool_result", call_id, ok: true, output: `wrote ${LIMITER_FIXED.length} bytes to src/limiter.rs` });
        await say(a, "The comparison is now inclusive, so the window resets at the boundary. I have not run the test suite: this demo has no shell tool.");
      } else {
        emit(a, { type: "tool_result", call_id, ok: false, output: "permission denied" });
        await say(a, "Understood. I have left src/limiter.rs unchanged.");
      }
      finish(a);
    });

    const b = makeSession("homelab-infra", 26 * 3600_000);
    emit(b, { type: "user_message", text: "/ls scripts" });
    const id = callId();
    emit(b, { type: "tool_call", call_id: id, tool: "list_dir", input: { path: "scripts" } });
    emit(b, { type: "tool_result", call_id: id, ok: true, output: "backup.sh" });
    emit(b, { type: "turn_finished" });
  }
  seed();

  // ---- simulated telemetry ----
  const sim = { cpu: 22, gpu: 8, cores: Array.from({ length: 16 }, () => 20), vram: 19.4, tps: 0, started: Date.now() };

  function host() {
    const busy = [...sessions.values()].some((s) => s.running && !s.pending.size);
    sim.cpu = walk(sim.cpu, 14, busy ? 30 : 6, busy ? 85 : 35);
    sim.gpu = walk(sim.gpu, 20, busy ? 70 : 2, busy ? 99 : 14);
    sim.cores = sim.cores.map((c) => walk(c, 40, 2, 100) * 0.5 + sim.cpu * 0.5);
    sim.tps = busy ? walk(sim.tps || 38, 8, 28, 52) : 0;
    return {
      hostname: "forge",
      os: "Linux 6.12 x86_64",
      uptime_s: 17 * 86400 + 5 * 3600 + Math.floor((Date.now() - sim.started) / 1000),
      load1: (sim.cpu / 100) * 6,
      cpu_pct: sim.cpu,
      cores: sim.cores,
      mem_used: 23.6 * GB + sim.cpu * 0.02 * GB,
      mem_total: 64 * GB,
      disk_used: 1.31 * 1024 * GB,
      disk_total: 2 * 1024 * GB,
      gpus: [{ name: "GPU 0", usage_pct: sim.gpu, mem_used: sim.vram * GB, mem_total: 24 * GB, temp_c: 41 + sim.gpu * 0.3 }],
      procs: [
        { name: "llm-server", cpu_pct: sim.cpu * 0.6, mem: 21.2 * GB },
        { name: "unchained", cpu_pct: 0.8 + sim.cpu * 0.03, mem: 0.09 * GB },
        { name: "rust-analyzer", cpu_pct: sim.cpu * 0.12, mem: 1.4 * GB },
        { name: "postgres", cpu_pct: 0.4, mem: 0.31 * GB },
        { name: "tailscaled", cpu_pct: 0.1, mem: 0.04 * GB },
      ],
    };
  }

  function info(s) {
    return { id: s.id, workspace: s.workspace, created_ms: s.created_ms, running: s.running, last_seq: s.events.length };
  }

  return {
    demo: true,
    sim,
    host,
    _sessions: sessions,
    async login() {
      return true;
    },
    async logout() {},
    async workspaces() {
      return [
        { name: "atlas-api", root: "/srv/repos/atlas-api" },
        { name: "homelab-infra", root: "/srv/repos/homelab-infra" },
      ];
    },
    async sessions() {
      return [...sessions.values()].map(info).sort((a, b) => b.created_ms - a.created_ms);
    },
    async createSession(workspace) {
      return info(makeSession(workspace, 0));
    },
    async status() {
      const all = [...sessions.values()];
      const events = all.reduce((n, s) => n + s.events.length, 0);
      return {
        host: host(),
        agent: {
          version: "0.0.1-demo",
          model: { name: "coder-32b-q5", ctx_used: 6200 + events * 410, ctx_max: 65536, tps: sim.tps },
          mode: "ask",
          sessions: all.length,
          running: all.filter((s) => s.running).length,
          pending: all.reduce((n, s) => n + s.pending.size, 0),
          clients: 2,
        },
      };
    },
    async files(workspace, path) {
      return list(workspace, path);
    },
    async readFile(workspace, path) {
      return (fs[workspace] || {})[path] ?? "Cannot open this file.";
    },
    connect(id, after, handlers) {
      const session = sessions.get(id);
      let open = true;
      const sub = (event) => open && handlers.onevent(event);
      setTimeout(() => {
        if (!open || !session) return;
        handlers.onopen();
        session.events.filter((e) => e.seq > after).forEach(sub);
        session.subs.add(sub);
      }, 250);
      return {
        send(message) {
          if (!session) return;
          if (message.type === "user_message") {
            if (session.running) handlers.onevent({ type: "rejected", reason: "a turn is already running in this session" });
            else turn(session, message.text);
          } else if (message.type === "permission_decision") {
            session.pending.get(message.call_id)?.(message.allow);
          } else if (message.type === "interrupt" && session.running) {
            session.stop = true;
            for (const resolve of [...session.pending.values()]) resolve(false);
          }
        },
        close() {
          open = false;
          session?.subs.delete(sub);
        },
      };
    },
  };
})();
