// The board's behaviour (SPEC §8), on top of pages that work without it:
// - relative times (`data-ago`) and running times (`data-since`) stay current;
// - a card or a step link opens the step drawer (`#step:<id>` in the address; Datastar turns
//   that into `$step` and streams the step's detail into the drawer); Escape or the close
//   button closes it, and focus goes back to the card;
// - hovering or focusing a block traces its edges (they light up and name their ports);
// - a status that changes flips its glyph once;
// - the board opens scrolled to the live frontier (the leftmost running, failed or stale block).

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

// ---- open at the live frontier --------------------------------------------------------------

const board = $(".board");
if (board && board.scrollWidth > board.clientWidth) {
  const live = $$(".plane .is-running, .plane .is-failed, .plane .is-stale");
  const next = live.length ? live : $$(".plane .is-pending");
  const left = Math.min(...next.map((n) => n.offsetLeft));
  if (Number.isFinite(left)) board.scrollLeft = Math.max(0, left - 72);
}
