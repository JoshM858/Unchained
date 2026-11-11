// A small, safe Markdown renderer for notes and artifacts. It builds DOM
// nodes and never parses HTML, so note text cannot inject markup. It handles
// what notes commonly use: frontmatter, headings, lists, quotes, code,
// emphasis, links, #tags and [[wikilinks]].
"use strict";

window.Markdown = (() => {
  const INLINE = /\[\[([^\]\n]+)\]\]|`([^`\n]+)`|\*\*([^*\n]+)\*\*|\*([^*\n]+)\*|\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)|(^|\s)#([A-Za-z][\w/-]*)/g;

  const el = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };

  // Split "target#heading|alias" into its target and the label to show.
  function wikilink(inner) {
    const [left, alias] = inner.split("|", 2);
    const target = left.split("#")[0].trim();
    return { target, label: (alias ?? target).trim() };
  }

  function inline(text, opts, parent) {
    INLINE.lastIndex = 0;
    let last = 0;
    let m;
    while ((m = INLINE.exec(text))) {
      if (m.index > last) parent.append(text.slice(last, m.index));
      if (m[1] !== undefined) {
        const { target, label } = wikilink(m[1]);
        parent.append(opts.link(target, label));
      } else if (m[2] !== undefined) {
        parent.append(el("code", "", m[2]));
      } else if (m[3] !== undefined) {
        parent.append(el("strong", "", m[3]));
      } else if (m[4] !== undefined) {
        parent.append(el("em", "", m[4]));
      } else if (m[5] !== undefined) {
        const a = el("a", "", m[5]);
        a.href = m[6];
        a.target = "_blank";
        a.rel = "noopener noreferrer";
        parent.append(a);
      } else if (m[8] !== undefined) {
        if (m[7]) parent.append(m[7]);
        parent.append(el("span", "tag", `#${m[8]}`));
      }
      last = INLINE.lastIndex;
    }
    if (last < text.length) parent.append(text.slice(last));
  }

  // Leading "---" block of simple "key: value" lines.
  function frontmatter(text) {
    if (!text.startsWith("---\n")) return { data: [], body: text };
    const lines = text.split("\n");
    const end = lines.indexOf("---", 1);
    if (end < 0) return { data: [], body: text };
    const data = [];
    for (const line of lines.slice(1, end)) {
      const i = line.indexOf(":");
      if (i <= 0) continue;
      data.push([line.slice(0, i).trim(), line.slice(i + 1).trim().replace(/^["']|["']$/g, "")]);
    }
    return { data, body: lines.slice(end + 1).join("\n") };
  }

  const isBlockStart = (line) =>
    /^\s*```/.test(line) ||
    /^#{1,6}\s/.test(line) ||
    /^>\s?/.test(line) ||
    /^\s*[-*+]\s+/.test(line) ||
    /^\s*\d+\.\s+/.test(line) ||
    /^\s*(---|\*\*\*|___)\s*$/.test(line);

  function blocks(lines, opts, parent) {
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (/^\s*```/.test(line)) {
        const code = [];
        i++;
        while (i < lines.length && !/^\s*```/.test(lines[i])) code.push(lines[i++]);
        i++;
        const pre = el("pre", "code");
        pre.append(el("code", "", code.join("\n")));
        parent.append(pre);
        continue;
      }
      if (!line.trim()) {
        i++;
        continue;
      }
      let m = /^(#{1,6})\s+(.*)$/.exec(line);
      if (m) {
        const h = el(`h${m[1].length}`);
        inline(m[2], opts, h);
        parent.append(h);
        i++;
        continue;
      }
      if (/^\s*(---|\*\*\*|___)\s*$/.test(line)) {
        parent.append(el("hr"));
        i++;
        continue;
      }
      if (/^>\s?/.test(line)) {
        const quoted = [];
        while (i < lines.length && /^>\s?/.test(lines[i])) quoted.push(lines[i++].replace(/^>\s?/, ""));
        const q = el("blockquote");
        blocks(quoted, opts, q);
        parent.append(q);
        continue;
      }
      if (/^\s*[-*+]\s+/.test(line) || /^\s*\d+\.\s+/.test(line)) {
        const ordered = /^\s*\d+\./.test(line);
        const list = el(ordered ? "ol" : "ul");
        const itemRe = ordered ? /^\s*\d+\.\s+(.*)$/ : /^\s*[-*+]\s+(.*)$/;
        while (i < lines.length && itemRe.test(lines[i])) {
          const li = el("li");
          inline(itemRe.exec(lines[i++])[1], opts, li);
          list.append(li);
        }
        parent.append(list);
        continue;
      }
      const p = el("p");
      const para = [];
      while (i < lines.length && lines[i].trim() && !isBlockStart(lines[i])) para.push(lines[i++]);
      para.forEach((text, n) => {
        if (n) p.append(el("br"));
        inline(text, opts, p);
      });
      parent.append(p);
    }
  }

  // Render Markdown text as a DocumentFragment. `options.link(target, label)`
  // returns the node for a [[wikilink]].
  function render(text, options = {}) {
    const opts = {
      ...options,
      link: options.link || ((target, label) => el("span", "wikilink", label)),
    };
    const frag = document.createDocumentFragment();
    const { data, body } = frontmatter(text);
    if (data.length) {
      const dl = el("dl", "kv frontmatter");
      for (const [k, v] of data) {
        dl.append(el("dt", "", k), el("dd", "", v));
      }
      frag.append(dl);
    }
    const holder = document.createElement("div");
    blocks(body.split("\n"), opts, holder);
    frag.append(...holder.childNodes);
    return frag;
  }

  return { render, links: (text) => [...text.matchAll(/\[\[([^\]\n]+)\]\]/g)].map((m) => wikilink(m[1]).target) };
})();
