// Shared HUD helpers: themes, formatters, graphs and the host telemetry
// panels. Used by the client (index.html) and the host console (host.html).
"use strict";

window.HUD = (() => {
  const $ = (id) => document.getElementById(id);
  const history = new Map();

  function store(key, value) {
    try {
      if (value === undefined) return localStorage.getItem(key);
      localStorage.setItem(key, value);
    } catch {
      /* storage can be unavailable; the UI works without it */
    }
    return null;
  }

  // One palette now (style.css). Kept so older pages can call it safely.
  function initTheme() {}

  function bytes(n) {
    if (n == null) return "—";
    const units = ["B", "K", "M", "G", "T"];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) {
      n /= 1024;
      i++;
    }
    return `${n >= 100 || i === 0 ? Math.round(n) : n.toFixed(1)}${units[i]}`;
  }

  function uptime(seconds) {
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    return d ? `${d}d ${h}h ${m}m` : `${h}h ${m}m`;
  }

  function text(id, value) {
    const el = $(id);
    if (el) el.textContent = value;
  }

  function bar(id, fraction) {
    const el = $(id);
    if (!el) return;
    const f = Math.max(0, Math.min(1, fraction || 0));
    el.firstElementChild.style.width = `${f * 100}%`;
    el.classList.toggle("hot", f > 0.9);
  }

  // Push a value onto a named rolling series and redraw its sparkline.
  function spark(id, value, max = 100, points = 60) {
    const svg = $(id);
    if (!svg) return;
    const series = history.get(id) || [];
    history.set(id, series);
    series.push(value);
    if (series.length > points) series.shift();
    const step = 100 / (points - 1);
    const offset = points - series.length;
    const coords = series.map((v, i) => {
      const y = 30 - (Math.max(0, Math.min(max, v)) / max) * 28 - 1;
      return `${((i + offset) * step).toFixed(1)},${y.toFixed(1)}`;
    });
    svg.setAttribute("viewBox", "0 0 100 30");
    svg.setAttribute("preserveAspectRatio", "none");
    let area = svg.querySelector("polygon");
    let line = svg.querySelector("polyline");
    if (!line) {
      const ns = "http://www.w3.org/2000/svg";
      area = svg.appendChild(document.createElementNS(ns, "polygon"));
      line = svg.appendChild(document.createElementNS(ns, "polyline"));
    }
    line.setAttribute("points", coords.join(" "));
    const first = (offset * step).toFixed(1);
    area.setAttribute("points", `${first},30 ${coords.join(" ")} 100,30`);
  }

  function clock() {
    const tick = () => {
      const now = new Date();
      text("clock", now.toLocaleTimeString([], { hour12: false }));
      text("date", now.toLocaleDateString([], { weekday: "short", year: "numeric", month: "short", day: "numeric" }));
    };
    tick();
    setInterval(tick, 1000);
  }

  // Fill the host panels (ids prefixed h-) from a /api/status host object.
  function renderHost(host) {
    if (!host) return;
    text("h-name", host.hostname || "—");
    text("h-os", host.os || "—");
    text("h-uptime", uptime(host.uptime_s || 0));
    text("h-load", (host.load1 ?? 0).toFixed(2));
    text("h-cpu", `${Math.round(host.cpu_pct || 0)}%`);
    spark("h-cpu-spark", host.cpu_pct || 0);
    const cores = $("h-cores");
    if (cores) {
      const list = host.cores || [];
      while (cores.children.length < list.length) cores.append(document.createElement("i"));
      while (cores.children.length > list.length) cores.lastChild.remove();
      list.forEach((v, i) => (cores.children[i].style.height = `${Math.max(4, v)}%`));
      text("h-core-count", `${list.length} cores`);
    }
    text("h-mem", `${bytes(host.mem_used)} / ${bytes(host.mem_total)}`);
    bar("h-mem-bar", host.mem_used / host.mem_total);
    text("h-disk", `${bytes(host.disk_used)} / ${bytes(host.disk_total)}`);
    bar("h-disk-bar", host.disk_used / host.disk_total);

    const gpu = (host.gpus || [])[0];
    const gpuPanel = $("h-gpu-panel");
    if (gpuPanel) gpuPanel.hidden = !gpu;
    if (gpu) {
      text("h-gpu-name", gpu.name);
      text("h-gpu", gpu.usage_pct == null ? "—" : `${Math.round(gpu.usage_pct)}%`);
      spark("h-gpu-spark", gpu.usage_pct || 0);
      text("h-vram", `${bytes(gpu.mem_used)} / ${bytes(gpu.mem_total)}`);
      bar("h-vram-bar", gpu.mem_total ? gpu.mem_used / gpu.mem_total : 0);
      text("h-gpu-temp", gpu.temp_c == null ? "" : `${Math.round(gpu.temp_c)}°C`);
    }

    const procs = $("h-procs");
    if (procs) {
      procs.replaceChildren(
        ...(host.procs || []).map((p) => {
          const row = document.createElement("tr");
          for (const value of [p.name, `${p.cpu_pct.toFixed(1)}%`, bytes(p.mem)]) {
            row.append(Object.assign(document.createElement("td"), { textContent: value }));
          }
          return row;
        }),
      );
    }
  }

  return { $, initTheme, bytes, uptime, text, bar, spark, clock, renderHost, store };
})();
