// The board's behaviour (SPEC §8), on top of pages that work without it:
// - relative times (`data-ago`) and running times (`data-since`) stay current;
// - a card or a step link opens the step drawer (`#step:<id>` in the address; Datastar turns
//   that into `$step` and streams the step's detail into the drawer); Escape or the close
//   button closes it, and focus goes back to the card;
// - hovering or focusing a block traces its edges (they light up and name their ports);
// - a status that changes flips its glyph once;
// - the edges between the cards are drawn here, and the arrow keys move between cards.

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

// ---- times ------------------------------------------------------------------------------

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

// ---- the step drawer --------------------------------------------------------------------

const drawer = $("#drawer");
let opener = null;
let stream = null;

// The drawer's stream: each call ends the previous one (Datastar's requestCancellation).
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

function currentStep() {
  return location.hash.startsWith("#step:") ? decodeURIComponent(location.hash.slice(6)) : "";
}

function markOpen(sid) {
  for (const n of $$(".node.open")) {
    if (n.id !== `n-${sid}`) n.classList.remove("open");
  }
  if (sid) document.getElementById(`n-${sid}`)?.classList.add("open");
}

function openStep() {
  const sid = currentStep();
  markOpen(sid);
  if (!drawer) return;
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
}

document.addEventListener("click", (evt) => {
  const a = evt.target.closest("a[data-step]");
  if (!a || !drawer || evt.button !== 0 || evt.metaKey || evt.ctrlKey || evt.shiftKey
      || evt.altKey) return;
  evt.preventDefault();
  if (!drawer.contains(a)) opener = a;
  location.hash = `step:${encodeURIComponent(a.dataset.step)}`;
});
document.addEventListener("keydown", (evt) => {
  if (evt.key === "Escape" && drawer && location.hash.startsWith("#step:")) window.sluiceClose();
});
window.addEventListener("hashchange", openStep);
openStep();

// A running step's stderr keeps its newest line in view unless the reader scrolled up.
if (drawer) {
  let pinned = true;
  drawer.addEventListener("scroll", (evt) => {
    const pre = evt.target;
    if (pre.classList?.contains("tail")) {
      pinned = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
    }
  }, true);
  new MutationObserver(() => {
    const pre = $("#step-detail pre.tail", drawer);
    if (pre && pinned) pre.scrollTop = pre.scrollHeight;
  }).observe(drawer, { childList: true, subtree: true, characterData: true });
}

// ---- tracing a block's edges -------------------------------------------------------------

function trace(node) {
  const plane = node?.closest(".plane");
  for (const p of $$(".plane.tracing")) {
    if (p !== plane) untrace(p);
  }
  if (!plane) return;
  const key = node.dataset.node;
  const near = new Set([key]);
  for (const el of $$("[data-from]", plane)) {
    const on = el.dataset.from === key || el.dataset.to === key;
    el.classList.toggle("on", on);
    if (on) near.add(el.dataset.from).add(el.dataset.to);
  }
  for (const n of $$(".node", plane)) n.classList.toggle("near", near.has(n.dataset.node));
  plane.classList.add("tracing");
}

function untrace(plane) {
  plane.classList.remove("tracing");
  for (const el of $$(".on, .near", plane)) el.classList.remove("on", "near");
}

document.addEventListener("pointerover", (evt) => {
  const node = evt.target.closest?.(".plane .node[data-node]");
  if (node) trace(node);
});
document.addEventListener("pointerout", (evt) => {
  const node = evt.target.closest?.(".plane .node[data-node]");
  if (node && !node.contains(evt.relatedTarget)) {
    const plane = node.closest(".plane");
    const focused = document.activeElement?.closest?.(".plane .node[data-node]");
    if (focused && plane.contains(focused)) trace(focused); else untrace(plane);
  }
});
document.addEventListener("focusin", (evt) => {
  const node = evt.target.closest?.(".plane .node[data-node]");
  if (node) trace(node);
  else for (const p of $$(".plane.tracing")) untrace(p);
});

// ---- a status change flips its glyph -------------------------------------------------------

const graph = $("#graph");
if (graph) {
  new MutationObserver((records) => {
    for (const r of records) {
      const el = r.target;
      if (r.type === "attributes" && el.classList?.contains("node")
          && /\bis-\w+/.exec(el.className)?.[0] !== /\bis-\w+/.exec(r.oldValue || "")?.[0]) {
        const g = $(".g", el);
        g?.classList.remove("flip");
        void g?.offsetWidth;
        g?.classList.add("flip");
      }
    }
    // a patch rewrites class attributes: keep the open card marked
    const sid = currentStep();
    if (sid && !document.getElementById(`n-${sid}`)?.classList.contains("open")) markOpen(sid);
  }).observe(graph, { attributes: true, attributeFilter: ["class"], attributeOldValue: true,
                      subtree: true, childList: true });
}

// ---- edges ---------------------------------------------------------------------------------
// The server lays the cards out in rows by depth; the edges (`data-edges` on the plane:
// [from, to, "output → input"]) are drawn here, from the bottom of a card to the top of the
// one it feeds, with an arrowhead. Several edges on one side of a card spread along it, in
// the order of the cards at their other ends. Redrawn when the board changes or resizes.

const SVG = "http://www.w3.org/2000/svg";

function svgEl(name, attrs) {
  const el = document.createElementNS(SVG, name);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}

function drawEdges(plane) {
  const svg = $("svg.edges", plane);
  if (!svg) return;
  let data = [];
  try { data = JSON.parse(plane.dataset.edges || "[]"); } catch { data = []; }
  const box = plane.getBoundingClientRect();
  const rect = new Map();
  for (const n of $$(".node[data-node]", plane)) rect.set(n.dataset.node, n.getBoundingClientRect());
  const ends = data.filter(([a, b]) => rect.has(a) && rect.has(b));
  const cx = (key) => rect.get(key).left + rect.get(key).width / 2;
  const spread = (key, others) => {  // an x on the card for each edge, in the others' order
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
  const marker = svgEl("marker", { id: "arrow", viewBox: "0 0 10 10", refX: "8", refY: "5",
                                   markerWidth: "8", markerHeight: "8",
                                   orient: "auto-start-reverse" });
  marker.append(svgEl("path", { d: "M0 1L9 5L0 9z" }));
  const defs = svgEl("defs", {});
  defs.append(marker);
  const wires = svgEl("g", { class: "wires" }), names = svgEl("g", { class: "names" });
  for (const [a, b, label] of ends) {
    const x1 = outX.get(a).get(b) - box.left, y1 = rect.get(a).bottom - box.top;
    const x2 = inX.get(b).get(a) - box.left, y2 = rect.get(b).top - box.top - 1;
    const dy = Math.max((y2 - y1) / 2, 14);
    const d = `M${x1.toFixed(1)} ${y1.toFixed(1)}C${x1.toFixed(1)} ${(y1 + dy).toFixed(1)} `
      + `${x2.toFixed(1)} ${(y2 - dy).toFixed(1)} ${x2.toFixed(1)} ${y2.toFixed(1)}`;
    wires.append(svgEl("path", { "data-from": a, "data-to": b, d, "marker-end": "url(#arrow)" }));
    const text = svgEl("text", { "data-from": a, "data-to": b, x: ((x1 + x2) / 2).toFixed(1),
                                 y: ((y1 + y2) / 2 + 4).toFixed(1) });
    text.textContent = label;
    names.append(text);
  }
  svg.replaceChildren(defs, wires, names);
}

let pending = 0;
function redraw() {
  cancelAnimationFrame(pending);
  pending = requestAnimationFrame(() => {
    for (const p of $$(".plane")) drawEdges(p);
    // new paths: keep the traced block lit
    const held = document.querySelector(".plane .node:hover, .plane .node:focus-visible");
    if (held) trace(held);
  });
}
redraw();
window.addEventListener("resize", redraw);
document.fonts?.ready.then(redraw);
if (graph) {
  const sizes = new ResizeObserver(redraw);
  const watch = () => { for (const p of $$(".plane", graph)) sizes.observe(p); };
  watch();
  new MutationObserver((records) => {
    // our own drawing lives in svg.edges: redraw only for changes to the board itself
    if (records.some((r) => !r.target.closest?.("svg.edges"))) { watch(); redraw(); }
  }).observe(graph, { childList: true, subtree: true, characterData: true, attributes: true,
                      attributeFilter: ["data-edges"] });
}

// ---- the Types switch: a value's type shows on demand, remembered in this browser ----------

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

// ---- moving between cards with the arrow keys ---------------------------------------------------

document.addEventListener("keydown", (evt) => {
  const here = document.activeElement?.closest?.(".plane .node[data-node]");
  if (!here || evt.altKey || evt.ctrlKey || evt.metaKey) return;
  const dir = { ArrowDown: [0, 1], ArrowUp: [0, -1], ArrowRight: [1, 0], ArrowLeft: [-1, 0] }[evt.key];
  if (!dir) return;
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
  if (best) { evt.preventDefault(); best.focus(); }
});
