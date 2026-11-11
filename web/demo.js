// In-browser simulation of the daemon, for showing the interface without
// one (`index.html?demo`, and the GitHub Pages demo). It speaks the same
// protocol and exposes the same backend methods as the real client, so the
// client code path is identical. Nothing here touches a real machine,
// vault or model.
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
      "AGENTS.md": "Run `cargo test` before proposing a change.\nKeep rate-limit behaviour documented in the vault.\n",
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

  // An Obsidian-style vault: a folder of Markdown notes.
  const vaults = {
    "p-atlas": {
      "Home.md": "---\ntags: [home]\nstatus: active\n---\n# Atlas\n\nStart here. The [[Limiter]] design is settled; see [[Ideas#later]].\nOpen questions live in [[Missing page]].\n\n#api #limits\n",
      "Limiter.md": "# Limiter\n\nFixed-window rate limiter for the gateway.\n\n- window: 60s\n- max: per key\n\nThe boundary case is covered by [[tests/limiter_test]].\n\n> Reset exactly at the boundary, not one tick after.\n",
      "Ideas.md": "# Ideas\n\n## later\n\nSliding windows, if fixed windows cause bursts. See [[Limiter]].\n",
      "Inbox/rate-limit-bug.md": "The CI failure is the window check using `>`.\n",
      ".obsidian/app.json": "{}",
    },
    "p-homelab": null,
  };

  // session scratch: session id -> { path: content }
  const scratch = {};
  // project id -> artifact name -> versions
  const artifacts = {
    "p-atlas": {
      "limiter-fix": [{ version: 1, kind: "markdown", text: "# Limiter fix\n\nUse `>=` at the window edge.\n", ts_ms: Date.now() - 3600_000 }],
    },
  };
  const archive = [];

  const projects = [
    { id: "p-atlas", name: "Atlas API", workspace: "atlas-api", vault: "/home/me/Vaults/Atlas", vault_mode: "read-write-ask", created_ms: Date.now() - 9 * 86400_000 },
    { id: "p-homelab", name: "Homelab", workspace: "homelab-infra", vault: null, vault_mode: "read-only", created_ms: Date.now() - 30 * 86400_000 },
  ];

  const sessions = new Map();
  let nextId = 0xa41f;

  function makeSession(project, ageMs) {
    const id = (nextId++).toString(16).padStart(4, "0") + "c9e27b10";
    const p = projects.find((x) => x.id === project);
    const session = {
      id,
      project,
      workspace: p.workspace,
      created_ms: Date.now() - ageMs,
      events: [],
      subs: new Set(),
      pending: new Map(),
      running: false,
      stop: false,
    };
    scratch[id] = {};
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

  // Permission for one call. "allow" runs at once, "ask" waits for the
  // operator, "deny" refuses. Returns true when the call may proceed.
  async function gate(session, call_id, tool, input, verdict) {
    if (verdict === "deny") return false;
    if (verdict === "ask") {
      emit(session, { type: "permission_requested", call_id, tool, input });
      const allowed = await new Promise((resolve) => session.pending.set(call_id, resolve));
      session.pending.delete(call_id);
      emit(session, { type: "permission_resolved", call_id, allowed, by: session.stop ? "interrupt" : "client" });
      return allowed;
    }
    return true;
  }

  function list(workspace, path) {
    const prefix = path === "." ? "" : path.replace(/\/$/, "") + "/";
    return listFrom(fs[workspace] || {}, prefix);
  }

  function listFrom(files, prefix) {
    const seen = new Map();
    for (const [file, content] of Object.entries(files)) {
      if (!file.startsWith(prefix)) continue;
      const rest = file.slice(prefix.length);
      const [head, ...tail] = rest.split("/");
      if (tail.length) seen.set(head, { name: head, dir: true, size: 0 });
      else seen.set(head, { name: head, dir: false, size: content.length });
    }
    return [...seen.values()].sort((a, b) => b.dir - a.dir || a.name.localeCompare(b.name));
  }

  const noteList = (pid) => Object.keys(vaults[pid] || {}).filter((p) => p.endsWith(".md") && !p.startsWith(".")).sort();
  const linkTarget = (inner) => inner.split("|")[0].split("#")[0].trim();
  const matches = (link, target) => {
    const t = target.replace(/\.md$/, "");
    const l = link.replace(/\.md$/, "");
    return l.includes("/") ? l === t : l === t.split("/").pop();
  };

  async function runTool(session, tool, input, verdict, action) {
    const call_id = callId();
    emit(session, { type: "tool_call", call_id, tool, input });
    await wait(350);
    const fail = (output) => emit(session, { type: "tool_result", call_id, ok: false, output });
    if (!(await gate(session, call_id, tool, input, verdict))) {
      fail(verdict === "deny" && tool === "write_note" ? "permission denied: this vault is read-only" : "permission denied");
      return false;
    }
    try {
      const output = await action(call_id);
      emit(session, { type: "tool_result", call_id, ok: true, output });
      return true;
    } catch (e) {
      fail(e.message);
      return false;
    }
  }

  const projectOf = (session) => projects.find((p) => p.id === session.project);
  const vaultVerdict = (mode) => ({ "read-only": "deny", "read-write-ask": "ask", "read-write": "allow" })[mode];

  async function turn(session, message) {
    session.running = true;
    emit(session, { type: "user_message", text: message });
    const [cmd, ...rest] = message.trim().split(/\s+/);
    const after = message.trim().slice(cmd.length).trim();
    const sub = (m) => (m ? emit(session, { type: "assistant_message", text: m }) : null);
    const path = (rest[0] || ".").replace(/^\.\//, "");
    const ws = fs[session.workspace];

    if (cmd === "/ls") {
      await runTool(session, "list_dir", { path: rest[0] || "." }, "allow", async () => {
        const entries = list(session.workspace, path);
        if (!entries.length && path !== ".") throw new Error(`${path}: No such file or directory`);
        return entries.map((e) => e.name + (e.dir ? "/" : "")).join("\n");
      });
    } else if (cmd === "/read" && rest[0]) {
      await runTool(session, "read_file", { path: rest[0] }, "allow", async () => {
        if (!(rest[0] in ws)) throw new Error(`${rest[0]}: No such file or directory`);
        return ws[rest[0]];
      });
    } else if (cmd === "/write" && rest[0]) {
      const content = after.slice(rest[0].length).trim();
      await runTool(session, "write_file", { path: rest[0], content }, "ask", async () => {
        ws[rest[0]] = content;
        return `wrote ${content.length} bytes to ${rest[0]}`;
      });
    } else if (cmd === "/scratch" && rest[0]) {
      const content = after.slice(rest[0].length).trim();
      await runTool(session, "scratch_write", { path: rest[0], content }, "allow", async () => {
        scratch[session.id][rest[0]] = content;
        return `wrote ${content.length} bytes to scratch/${rest[0]}`;
      });
    } else if (cmd === "/scratch-ls") {
      await runTool(session, "scratch_list", { path: "." }, "allow", async () =>
        Object.keys(scratch[session.id]).sort().join("\n") || "(empty)");
    } else if (cmd === "/promote" && rest[1]) {
      const to = rest[1];
      const content = scratch[session.id][rest[0]];
      await runTool(session, "promote_scratch", { from: rest[0], to }, "ask", async () => {
        if (content === undefined) throw new Error(`${rest[0]}: No such file`);
        ws[to] = content;
        return `promoted ${content.length} bytes to ${to}`;
      });
    } else if (cmd === "/artifact" && rest[2]) {
      const [name, kind] = rest;
      const content = after.split(/\s+/).slice(2).join(" ");
      await runTool(session, "create_artifact", { name, kind, content }, "allow", async () => {
        const pid = session.project;
        artifacts[pid] = artifacts[pid] || {};
        const versions = artifacts[pid][name] || [];
        versions.push({ version: versions.length + 1, kind, text: content, ts_ms: Date.now() });
        artifacts[pid][name] = versions;
        return `saved artifact ${name} as version ${versions.length}`;
      });
    } else if (cmd === "/note" && rest[0]) {
      const project = projectOf(session);
      const body = after + "\n";
      const notePath = `Inbox/${after.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "note"}.md`;
      const verdict = project.vault ? vaultVerdict(project.vault_mode) : "deny";
      await runTool(session, "write_note", { path: notePath, content: body }, verdict, async () => {
        vaults[project.id][notePath] = body;
        return `wrote ${body.length} bytes to ${notePath}`;
      });
    } else if (cmd === "/find" && rest[0]) {
      await runTool(session, "search_notes", { query: after }, "allow", async () => {
        const hits = await searchVault(projectOf(session).id, after);
        return hits.length ? hits.map((h) => `${h.path}:${h.line}: ${h.snippet}`).join("\n") : "no matches";
      });
    } else if (cmd === "/context") {
      await wait(400);
      sub(projectOf(session).workspace === "atlas-api"
        ? "Project instructions are loaded from AGENTS.md (2 lines)."
        : "This project has no AGENTS.md or CLAUDE.md.");
    } else {
      await wait(500);
      sub("This is a simulated agent running in your browser: no model and no machine are connected. Try /ls, /read <file>, /write <file> <text>, /scratch <name> <text>, /promote <name> <path>, /artifact <name> <html|svg|markdown|code> <content>, /note <text>, /find <words>, /context.");
    }
    finish(session);
  }

  function finish(session) {
    session.running = false;
    session.stop = false;
    emit(session, { type: "turn_finished" });
  }

  async function searchVault(pid, query) {
    const q = query.trim().toLowerCase();
    const hits = [];
    for (const path of noteList(pid)) {
      vaults[pid][path].split("\n").forEach((line, i) => {
        if (line.toLowerCase().includes(q)) hits.push({ path, line: i + 1, snippet: line.trim() });
      });
    }
    return hits;
  }

  // Session 1 opens mid-task, paused on an approval.
  async function seed() {
    const a = makeSession("p-atlas", 42 * 60_000);
    a.running = true;
    scratch[a.id] = { "draft-window.md": "# Window edge\n\nTry `>=` and check the boundary test.\n" };
    artifacts["p-atlas"]["window-chart"] = [
      {
        version: 1,
        kind: "html",
        ts_ms: Date.now() - 1800_000,
        text: "<!doctype html><meta charset=utf-8><style>body{font:14px monospace;margin:16px;background:#0b1116;color:#cfe6ea}</style><h3>Requests per window</h3><svg width=320 height=120><polyline fill=none stroke=#7fe3f0 stroke-width=2 points='0,100 40,88 80,92 120,40 160,70 200,30 240,60 280,20 320,44'/></svg>",
      },
    ];
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
        await wait(500);
        emit(a, { type: "assistant_message", text: "The comparison is now inclusive, so the window resets at the boundary. I have not run the test suite: this demo has no shell tool." });
      } else {
        emit(a, { type: "tool_result", call_id, ok: false, output: "permission denied" });
        await wait(500);
        emit(a, { type: "assistant_message", text: "Understood. I have left src/limiter.rs unchanged." });
      }
      finish(a);
    });

    const b = makeSession("p-homelab", 26 * 3600_000);
    emit(b, { type: "user_message", text: "/ls scripts" });
    const id = callId();
    emit(b, { type: "tool_call", call_id: id, tool: "list_dir", input: { path: "scripts" } });
    emit(b, { type: "tool_result", call_id: id, ok: true, output: "backup.sh" });
    emit(b, { type: "turn_finished" });

    // Something from last month, already archived.
    archive.push({
      name: "a3f0c9e27b10-archive.tar.gz",
      session: "a3f0c9e27b10",
      archived_ms: Date.now() - 9 * 86400_000,
      files: 2,
      bytes: 1800,
      content: { "backup-notes.md": "# Backup notes\n\nRotate the NAS snapshots monthly.\n", "retention.txt": "30 days\n" },
    });
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
    return { id: s.id, project: s.project, workspace: s.workspace, created_ms: s.created_ms, running: s.running, last_seq: s.events.length };
  }

  const projectView = (p) => ({ ...p });

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
    async projects() {
      return { projects: projects.map(projectView), frame_base: "demo" };
    },
    async createProject(body) {
      if (!body.name) throw new Error("a project needs a name");
      const project = {
        id: "p-" + Math.random().toString(36).slice(2, 8),
        name: body.name,
        workspace: body.workspace,
        vault: body.vault || null,
        vault_mode: body.vault_mode || "read-only",
        created_ms: Date.now(),
      };
      projects.push(project);
      vaults[project.id] = project.vault ? {} : null;
      return projectView(project);
    },
    async updateProject(id, body) {
      const p = projects.find((x) => x.id === id);
      if (!p) throw new Error(`no project '${id}'`);
      if (body.vault !== undefined) {
        p.vault = body.vault;
        if (body.vault && !vaults[id]) vaults[id] = {};
      }
      if (body.vault_mode) p.vault_mode = body.vault_mode;
      return projectView(p);
    },
    async instructions(id) {
      const p = projects.find((x) => x.id === id);
      const file = (fs[p.workspace] || {})["AGENTS.md"] !== undefined ? "AGENTS.md" : null;
      return file ? { file, text: fs[p.workspace]["AGENTS.md"] } : { file: null, text: "" };
    },
    async notes(pid) {
      if (!vaults[pid]) throw new Error("this project has no notes vault");
      return noteList(pid).map((path) => ({ path, title: path.split("/").pop().replace(/\.md$/, "") }));
    },
    async note(pid, path) {
      if (!vaults[pid] || vaults[pid][path] === undefined) throw new Error("no such note");
      return vaults[pid][path];
    },
    async backlinks(pid, path) {
      const out = [];
      for (const other of noteList(pid)) {
        if (other === path) continue;
        const links = [...vaults[pid][other].matchAll(/\[\[([^\]\n]+)\]\]/g)].map((m) => linkTarget(m[1]));
        if (links.some((l) => matches(l, path))) out.push(other);
      }
      return out;
    },
    async searchNotes(pid, query) {
      if (!vaults[pid]) throw new Error("this project has no notes vault");
      return searchVault(pid, query);
    },
    async artifacts(pid) {
      return Object.entries(artifacts[pid] || {}).map(([name, versions]) => ({
        name,
        kind: versions[versions.length - 1].kind,
        versions: versions.map((v) => ({ version: v.version, bytes: v.text.length, ts_ms: v.ts_ms })),
      }));
    },
    async artifact(pid, name, version) {
      const versions = (artifacts[pid] || {})[name];
      if (!versions) throw new Error(`no artifact named '${name}'`);
      const v = versions.find((x) => x.version === (version || versions.length)) || versions[versions.length - 1];
      const meta = { name, kind: v.kind, versions: versions.map((x) => ({ version: x.version, bytes: x.text.length, ts_ms: x.ts_ms })) };
      return { meta, version: v.version, text: v.text };
    },
    async sessions(project) {
      return [...sessions.values()].filter((s) => !project || s.project === project).map(info).sort((a, b) => b.created_ms - a.created_ms);
    },
    async createSession(project) {
      return info(makeSession(project, 0));
    },
    async deleteSession(id) {
      const s = sessions.get(id);
      if (!s) throw new Error("no such session");
      if (s.running) throw new Error("the session is running; interrupt it first");
      sessions.delete(id);
      delete scratch[id];
    },
    async scratchList(id, path) {
      const files = scratch[id];
      if (!files) return [];
      return listFrom(files, path === "." ? "" : path.replace(/\/$/, "") + "/");
    },
    async scratchRead(id, path) {
      const files = scratch[id] || {};
      if (!(path in files)) throw new Error("no such file");
      return files[path];
    },
    async archives() {
      return archive.map(({ content, ...a }) => a);
    },
    async restoreArchive(name) {
      const i = archive.findIndex((a) => a.name === name);
      if (i < 0) throw new Error(`no archive named ${name}`);
      const [a] = archive.splice(i, 1);
      scratch[a.session] = { ...(scratch[a.session] || {}), ...a.content };
      return { session: a.session };
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
