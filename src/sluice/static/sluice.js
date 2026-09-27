// The dashboard's behaviour (SPEC §8), on top of pages that work without it. Three Rocket
// components (Datastar's web components, from the bundle the page streams with), each in the
// light DOM around what the server rendered:
// - <sluice-board edges="[[from, to, names], ...]">: draws the edges between the cards, around
//   the cards they would cross; hovering or focusing a card traces its edges and names them;
//   the arrow keys move between cards; a status that changes flips its glyph once.
// - <sluice-drawer>: the step drawer. A step link opens it (`#step:<id>` in the address;
//   Datastar turns that into `$step` and streams the step's detail in); Escape, the close
//   button or the scrim close it, and focus goes back to the link. A running step's log keeps
//   its newest line in view unless the reader scrolled up.
// - <sluice-thread project thread last>: counts the messages this browser has not seen on a
//   thread, marks them when it is opened, and opens the thread the address names.
// On every page: relative times (`data-ago`) and running times (`data-since`) stay current,
// and the Types switch shows the types of values.

import { rocket } from
  "https://cdn.jsdelivr.net/gh/starfederation/datastar@v1.0.4/bundles/datastar-rocket.js";

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

// ---- times ---------------------------------------------------------------------------------

function dur(seconds) {
  if (seconds < 10) return `${Math.max(seconds, 0).toFixed(1)}s`.replace(".0s", "s");
  let s = Math.floor(seconds);
  if (s < 60) return `${s}s`;
  const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600);
  const m = Math.floor(s % 3600 / 60), sec = s % 60;
  const parts = [[d, "d"], [h, "h"], [m, "m"]].concat(!d && !h ? [[sec, "s"]] : []);
  return parts.filter(([n]) => n).map(([n, u]) => `${n}${u}`).join(" ") || "0s";
}

function ago(seconds) {
  for (const [unit, size] of [["d", 86400], ["h", 3600], ["m", 60]]) {
    if (seconds >= size) return `${Math.floor(seconds / size)}${unit} ago`;
  }
  return "just now";
}

function tick() {
  const now = Date.now();
  for (const t of $$("time[data-since]")) {
    t.textContent = dur((now - Date.parse(t.dataset.since)) / 1000);
  }
  for (const t of $$("time[data-ago]")) {
    t.textContent = ago((now - Date.parse(t.getAttribute("datetime"))) / 1000);
  }
}
setInterval(tick, 5000);

// ---- the open step ---------------------------------------------------------------------------

function currentStep() {
  return location.hash.startsWith("#step:") ? decodeURIComponent(location.hash.slice(6)) : "";
}

function markOpen(sid) {
  for (const n of $$(".node.open")) {
    if (n.id !== `n-${sid}`) n.classList.remove("open");
  }
  if (sid) document.getElementById(`n-${sid}`)?.classList.add("open");
}

// ---- <sluice-board> ---------------------------------------------------------------------------
// Each edge leaves the bottom of a card and enters the top of the card it feeds, ending in an
// arrowhead. Several edges on one side of a card spread along it, in the order of the cards at
// their other ends. An edge that passes rows of cards on its way runs through the nearest gap
// in each (edges sharing a gap sit side by side), so it never hides behind a card.

const SVG = "http://www.w3.org/2000/svg";
const HEAD_W = 3.5, HEAD_H = 6;  // the arrowhead: a shape of its own, which lights up with its edge
const CLEAR = 7;                 // the least space between an edge and a card it passes
const SIDE = 5;                  // between edges sharing a gap
const LEFT = 40;                 // how much nearer a gap on the left must be: bypasses keep right

function svgEl(name, attrs) {
  const el = document.createElementNS(SVG, name);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}

// The rows of cards, top to bottom: cards whose boxes overlap in height share one.
function bands(boxes) {
  const out = [];
  for (const r of [...boxes].sort((a, b) => a.top - b.top)) {
    const last = out[out.length - 1];
    if (last && r.top < last.bottom - 2) {
      last.bottom = Math.max(last.bottom, r.bottom);
      last.spans.push([r.left, r.right]);
    } else {
      out.push({ top: r.top, bottom: r.bottom, spans: [[r.left, r.right]] });
    }
  }
  return out;
}

// The x to pass a row at: in the free gap nearest `want` (one on the right when it is about as
// near, so the edges that pass a card run together on one side), beside any edge already there.
function passAt(band, want, lo, hi, used, key) {
  const gaps = [];
  let x = lo;
  for (const [l, r] of band.spans.map(([l, r]) => [l - CLEAR, r + CLEAR])
    .sort((a, b) => a[0] - b[0])) {
    if (l > x) gaps.push([x, l]);
    x = Math.max(x, r);
  }
  if (hi > x) gaps.push([x, hi]);
  let best = null;
  for (const [l, r] of gaps) {
    const at = Math.min(Math.max(want, l), r);
    const cost = Math.abs(at - want) + (at < want ? LEFT : 0);
    if (!best || cost < best.cost) best = { at, l, r, cost };
  }
  if (!best) return want;
  const slot = `${key}:${Math.round(best.l)}`;
  const n = used.get(slot) || 0;
  used.set(slot, n + 1);
  const side = best.at <= (best.l + best.r) / 2 ? 1 : -1;  // fan out into the gap
  return Math.min(Math.max(best.at + side * n * SIDE, best.l), best.r);
}

function drawEdges(host, data) {
  const plane = $(".plane", host), svg = $("svg.edges", host);
  if (!plane || !svg) return;
  const box = plane.getBoundingClientRect();
  const rect = new Map();
  for (const n of $$(".node[data-node]", plane)) {
    const r = n.getBoundingClientRect();
    rect.set(n.dataset.node, { left: r.left - box.left, right: r.right - box.left,
                               top: r.top - box.top, bottom: r.bottom - box.top,
                               width: r.width });
  }
  const rows = bands(rect.values());
  const lo = -CLEAR * 2, hi = box.width + CLEAR * 2;
  const ends = (Array.isArray(data) ? data : []).filter(([a, b]) => rect.has(a) && rect.has(b));
  const cx = (key) => rect.get(key).left + rect.get(key).width / 2;
  const spread = (key, others) => {
    const r = rect.get(key), sorted = [...others].sort((p, q) => cx(p) - cx(q));
    return new Map(sorted.map((o, i) => [o, r.left + r.width * (i + 1) / (sorted.length + 1)]));
  };
  const outs = new Map(), ins = new Map();
  for (const [a, b] of ends) {
    if (!outs.has(a)) outs.set(a, []);
    if (!ins.has(b)) ins.set(b, []);
    outs.get(a).push(b);
    ins.get(b).push(a);
  }
  const outX = new Map([...outs].map(([k, v]) => [k, spread(k, v)]));
  const inX = new Map([...ins].map(([k, v]) => [k, spread(k, v)]));
  const used = new Map();
  const wires = svgEl("g", { class: "wires" }), names = svgEl("g", { class: "names" });
  const f = (n) => n.toFixed(1);
  for (const [a, b, label] of ends) {
    const x1 = outX.get(a).get(b), y1 = rect.get(a).bottom;
    const x2 = inX.get(b).get(a), tip = rect.get(b).top - 1;
    const y2 = tip - HEAD_H;  // the line ends straight down, into the head's base
    const pts = [[x1, y1]];
    rows.forEach((row, i) => {
      if (row.top <= y1 + 1 || row.bottom >= tip - 1) return;  // only the rows in between
      const t = ((row.top + row.bottom) / 2 - y1) / (tip - y1);
      const x = passAt(row, x1 + (x2 - x1) * t, lo, hi, used, i);
      pts.push([x, row.top - 4], [x, row.bottom + 4]);
    });
    pts.push([x2, y2]);
    let d = `M${f(x1)} ${f(y1)}`;
    for (let i = 1; i < pts.length; i++) {
      const [xa, ya] = pts[i - 1], [xb, yb] = pts[i];
      if (i % 2 === 0) {  // down through a row's gap
        d += `L${f(xb)} ${f(yb)}`;
      } else {
        const dy = Math.max((yb - ya) / 2, pts.length === 2 ? 12 : 4);
        d += `C${f(xa)} ${f(ya + dy)} ${f(xb)} ${f(yb - dy)} ${f(xb)} ${f(yb)}`;
      }
    }
    const attrs = { "data-from": a, "data-to": b, d };
    if (label === "after") attrs.class = "order";  // an ordering edge carries no value
    wires.append(svgEl("path", attrs));
    wires.append(svgEl("path", { "data-from": a, "data-to": b, class: "head",
                                 d: `M${f(x2 - HEAD_W)} ${f(y2)}L${f(x2)} ${f(tip)}`
                                    + `L${f(x2 + HEAD_W)} ${f(y2)}z` }));
    // its names twice: by the far end from whichever card is traced, so the names of a card's
    // edges spread out over the cards around it instead of piling up on it
    const near = [[pts[0], pts[1], "from"], [pts[pts.length - 2], pts[pts.length - 1], "to"]];
    for (const [[xa, ya], [xb, yb], end] of near) {
      const text = svgEl("text", { "data-from": a, "data-to": b, "data-end": end,
                                   x: f((xa + xb) / 2), y: f((ya + yb) / 2 + 4) });
      text.textContent = label;
      names.append(text);
    }
  }
  svg.replaceChildren(wires, names);
}

function trace(host, node) {
  const key = node.dataset.node;
  const near = new Set([key]);
  for (const el of $$("[data-from]", host)) {
    const from = el.dataset.from === key, to = el.dataset.to === key;
    const end = el.dataset.end;  // a name shows by the other card
    const on = end ? (from && end === "to") || (to && end === "from") : from || to;
    el.classList.toggle("on", on);
    if (from || to) near.add(el.dataset.from).add(el.dataset.to);
  }
  for (const n of $$(".node", host)) n.classList.toggle("near", near.has(n.dataset.node));
  $(".plane", host)?.classList.add("tracing");
}

function untrace(host) {
  $(".plane", host)?.classList.remove("tracing");
  for (const el of $$(".on, .near", host)) el.classList.remove("on", "near");
}

function nearestCard(here, evt) {
  const dir = { ArrowDown: [0, 1], ArrowUp: [0, -1], ArrowRight: [1, 0],
                ArrowLeft: [-1, 0] }[evt.key];
  if (!dir) return null;
  const r = here.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2;
  let best = null, bestScore = Infinity;
  for (const n of $$(".node[data-node]", here.closest(".plane"))) {
    if (n === here) continue;
    const q = n.getBoundingClientRect(), nx = q.left + q.width / 2, ny = q.top + q.height / 2;
    const along = dir[0] ? (nx - x) * dir[0] : (ny - y) * dir[1];
    const across = dir[0] ? Math.abs(ny - y) : Math.abs(nx - x);
    if (along <= 4 || (dir[0] && across > r.height / 2)) continue;  // left/right: same row
    const score = along + across * 2;
    if (score < bestScore) { best = n; bestScore = score; }
  }
  return best;
}

rocket("sluice-board", {
  mode: "light",
  props: ({ json }) => ({ edges: json.default([]) }),
  setup({ host, props, observeProps, cleanup }) {
    let frame = 0;
    const redraw = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        drawEdges(host, props.edges);
        const held = $(".node:hover, .node:focus-visible", host);  // new paths: keep it lit
        if (held) trace(host, held);
      });
    };
    observeProps(redraw, "edges");
    const sizes = new ResizeObserver(redraw);
    sizes.observe(host);
    document.fonts?.ready.then(redraw);
    // a patch of the board: redraw, keep the open card marked, flip a glyph whose status moved
    const changes = new MutationObserver((records) => {
      let board = false;
      for (const r of records) {
        if (r.target.closest?.("svg.edges")) continue;
        board = true;
        const el = r.target;
        if (r.type === "attributes" && el.classList?.contains("node")
            && /\bis-\w+/.exec(el.className)?.[0] !== /\bis-\w+/.exec(r.oldValue || "")?.[0]) {
          const g = $(".g", el);
          g?.classList.remove("flip");
          void g?.offsetWidth;
          g?.classList.add("flip");
        }
      }
      if (!board) return;
      const sid = currentStep();
      if (sid && !document.getElementById(`n-${sid}`)?.classList.contains("open")) markOpen(sid);
      redraw();
    });
    changes.observe(host, { childList: true, subtree: true, characterData: true,
                            attributes: true, attributeFilter: ["class"],
                            attributeOldValue: true });
    const card = (evt) => evt.target.closest?.(".node[data-node]");
    const over = (evt) => { const n = card(evt); if (n) trace(host, n); };
    const out = (evt) => {
      const n = card(evt);
      if (!n || n.contains(evt.relatedTarget)) return;
      const focused = document.activeElement?.closest?.(".node[data-node]");
      if (focused && host.contains(focused)) trace(host, focused); else untrace(host);
    };
    const focus = (evt) => { const n = card(evt); if (n) trace(host, n); else untrace(host); };
    const keys = (evt) => {
      const here = card(evt);
      if (!here || evt.altKey || evt.ctrlKey || evt.metaKey) return;
      const next = nearestCard(here, evt);
      if (next) { evt.preventDefault(); next.focus(); }
    };
    host.addEventListener("pointerover", over);
    host.addEventListener("pointerout", out);
    host.addEventListener("focusin", focus);
    host.addEventListener("focusout", (evt) => { if (!host.contains(evt.relatedTarget)) untrace(host); });
    host.addEventListener("keydown", keys);
    markOpen(currentStep());
    cleanup(() => {
      cancelAnimationFrame(frame);
      sizes.disconnect();
      changes.disconnect();
    });
  },
  onFirstRender({ host, props }) {
    drawEdges(host, props.edges);
  },
});

// ---- <sluice-drawer> --------------------------------------------------------------------------

rocket("sluice-drawer", {
  mode: "light",
  setup({ host, cleanup }) {
    const drawer = $("#drawer", host);
    let opener = null, stream = null;
    // the drawer's stream: each call ends the previous one (Datastar's requestCancellation)
    window.sluiceStream = () => {
      stream?.abort();
      stream = new AbortController();
      return stream;
    };
    window.sluiceClose = () => {
      if (!location.hash.startsWith("#step:")) return;
      history.pushState(null, "", location.pathname + location.search);
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    };
    const open = () => {
      const sid = currentStep();
      markOpen(sid);
      if (!sid) {
        opener?.focus({ preventScroll: true });
        opener = null;
        return;
      }
      const detail = $("#step-detail", drawer);
      if (detail && detail.dataset.step !== sid) {
        detail.replaceChildren();  // no stale detail while the new one streams in
        detail.dataset.step = sid;
      }
      requestAnimationFrame(() => drawer.focus({ preventScroll: true }));
    };
    const click = (evt) => {
      const a = evt.target.closest?.("a[data-step]");
      if (!a || evt.button !== 0 || evt.metaKey || evt.ctrlKey || evt.shiftKey
          || evt.altKey) return;
      evt.preventDefault();
      if (!drawer.contains(a)) opener = a;
      location.hash = `step:${encodeURIComponent(a.dataset.step)}`;
    };
    const escape = (evt) => {
      if (evt.key === "Escape" && location.hash.startsWith("#step:")) window.sluiceClose();
    };
    let pinned = true;  // the log follows its newest line until the reader scrolls up
    const scrolled = (evt) => {
      const pre = evt.target;
      if (pre.classList?.contains("tail")) {
        pinned = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
      }
    };
    const follow = new MutationObserver(() => {
      const pre = $("#step-detail pre.tail", drawer);
      if (pre && pinned) pre.scrollTop = pre.scrollHeight;
    });
    follow.observe(drawer, { childList: true, subtree: true, characterData: true });
    document.addEventListener("click", click);
    document.addEventListener("keydown", escape);
    window.addEventListener("hashchange", open);
    drawer.addEventListener("scroll", scrolled, true);
    open();
    cleanup(() => {
      document.removeEventListener("click", click);
      document.removeEventListener("keydown", escape);
      window.removeEventListener("hashchange", open);
      follow.disconnect();
      delete window.sluiceStream;
      delete window.sluiceClose;
    });
  },
});

// ---- <sluice-thread> --------------------------------------------------------------------------
// What this browser has seen of each thread is the seq of its last message then, kept in
// localStorage (a convenience of this viewer: without it, nothing is marked). A thread it has
// never seen counts as read. The marks on new messages are a stylesheet of their seqs, so a
// patch of the list, which rewrites the messages, keeps them.

const SEEN = "sluice.seen";
const fresh = new Map();  // thread element id -> the seqs marked new while it is open
const marks = document.head.appendChild(document.createElement("style"));

function seen() {
  try { return JSON.parse(localStorage.getItem(SEEN) || "{}") || {}; } catch { return {}; }
}

function remember(key, seq) {
  try {
    const all = seen();
    all[key] = seq;
    localStorage.setItem(SEEN, JSON.stringify(all));
  } catch { /* no storage: nothing is remembered */ }
}

function restyle() {
  const sel = [...fresh].flatMap(([id, seqs]) => seqs.map(
    (s) => `#${CSS.escape(id)} li.m[data-seq="${s}"] .m-from::before`));
  marks.textContent = sel.length ? `${sel.join(",\n")} { content: ""; display: inline-block;
    width: 6px; height: 6px; margin: 0 6px 1px 0; border-radius: 50%;
    background: var(--status-active); }` : "";
}

rocket("sluice-thread", {
  mode: "light",
  props: ({ string, number }) => ({ project: string, thread: string, last: number.default(0) }),
  setup({ host, props, observeProps, cleanup }) {
    const key = () => `${props.project}/${props.thread}`;
    const details = () => $("details.thread", host);
    const update = () => {
      const d = details();
      if (!d) return;
      const known = seen()[key()];
      if (known === undefined) remember(key(), props.last);  // first sight: nothing is new
      const since = known ?? props.last;
      const unseen = $$("li.m[data-seq]", d).map((li) => Number(li.dataset.seq))
        .filter((s) => s > since);
      if (d.open && unseen.length) {  // read now: marked while it stays open
        fresh.set(d.id, [...(fresh.get(d.id) || []), ...unseen]);
        remember(key(), props.last);
        restyle();
      }
      const n = d.open ? 0 : unseen.length;
      host.classList.toggle("unseen", n > 0);
      const pill = $(".th-new", d);
      if (pill) pill.textContent = n ? `${n} new` : "";
    };
    const toggled = () => {
      const d = details();
      if (d && !d.open && fresh.delete(d.id)) restyle();
      update();
    };
    const named = () => {
      const d = details();
      if (d && location.hash === `#${d.id}`) {
        d.open = true;
        d.scrollIntoView({ block: "start" });
      }
    };
    observeProps(update, "last");
    host.addEventListener("toggle", toggled, true);
    window.addEventListener("hashchange", named);
    named();
    update();
    cleanup(() => window.removeEventListener("hashchange", named));
  },
});

// ---- the Types switch: a value's type shows on demand, remembered in this browser -----------

function setTypes(on) {
  document.documentElement.classList.toggle("show-types", on);
  for (const b of $$(".types-toggle")) b.setAttribute("aria-pressed", String(on));
}
let typesOn = false;
try { typesOn = localStorage.getItem("sluice.types") === "1"; } catch { /* no storage */ }
setTypes(typesOn);
document.addEventListener("click", (evt) => {
  if (!evt.target.closest?.(".types-toggle")) return;
  typesOn = !typesOn;
  setTypes(typesOn);
  try { localStorage.setItem("sluice.types", typesOn ? "1" : "0"); } catch { /* no storage */ }
});
// a patch brings new switches: keep them in step
new MutationObserver(() => {
  for (const b of $$(".types-toggle")) {
    if (b.getAttribute("aria-pressed") !== String(typesOn)) b.setAttribute("aria-pressed", String(typesOn));
  }
}).observe(document.body, { childList: true, subtree: true });
