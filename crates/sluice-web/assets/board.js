// The project page with its board beside the plan (docs("board")). Without script both
// sections show (side by side from 1280px, one after the other below), the board's tools apply
// with their Apply button and a Button posts the page. With script:
// - the view switch: Plan · Both · Board from 1280px, Plan · Board below; each remembered per
//   project (localStorage), wide and narrow apart;
// - the splitter between plan and board: drag, arrow keys (16px, 64px with Shift), Home/End,
//   double-click to reset; the board's width remembered per project;
// - the description's "More", remembered per project;
// - the board tools apply as they change, the search as one types: the address follows
//   (history.replaceState) and the page's stream restarts with the new query, so its patches
//   draw the board as filtered;
// - a board Button's say is sent without leaving the page, the answer shown under the board.
const WIDE = matchMedia("(min-width: 1280px)");
const MIN_BOARD = 320, MIN_PLAN = 560, STEP = 16, BIG_STEP = 64;

const store = {
  get(key) { try { return localStorage.getItem(key); } catch { return null; } },
  set(key, value) {
    try {
      if (value == null) localStorage.removeItem(key); else localStorage.setItem(key, value);
    } catch { /* private mode: not remembered */ }
  },
};
const page = () => document.querySelector("#project-board");

// ---- the view: Plan · Both · Board ----------------------------------------------------------

const chosen = {};  // this tab's choice, should storage refuse it
const mode = () => (WIDE.matches ? "wide" : "narrow");
const viewKey = (m, id) => (m === "wide" ? `sluice.view-wide.${id}` : `sluice.view.${id}`);
function viewOf(p) {
  if (!p.classList.contains("has-panel")) return "plan";
  const m = mode();
  const views = m === "wide" ? ["plan", "both", "board"] : ["plan", "board"];
  const v = chosen[m] ?? store.get(viewKey(m, p.dataset.project));
  return views.includes(v) ? v : m === "wide" ? "both" : "plan";
}

/** Put the page in its view and the switch, splitter and description in step with it. */
function sync() {
  const p = page();
  if (!p) return;
  const view = viewOf(p);
  if (p.dataset.view !== view) p.dataset.view = view;
  for (const tab of p.querySelectorAll("[data-view-tab]")) {
    const on = String(tab.dataset.viewTab === view);
    if (tab.getAttribute("aria-pressed") !== on) tab.setAttribute("aria-pressed", on);
  }
  restoreWidth(p);
  restoreAbout(p);
}

document.addEventListener("click", event => {
  const tab = event.target.closest?.("[data-view-tab]");
  const p = tab?.closest("#project-board");
  if (!p) return;
  chosen[mode()] = tab.dataset.viewTab;
  store.set(viewKey(mode(), p.dataset.project), tab.dataset.viewTab);
  sync();
  frameSplitter();
});
WIDE.addEventListener("change", () => { sync(); frameSplitter(); });

// ---- the splitter -----------------------------------------------------------------------------

const widthKey = id => `sluice.boardw.${id}`;
const splitter = () => document.querySelector("#project-board .splitter");
const boardPane = () => document.querySelector("#board-pane");

/** The board's least and greatest width on this page now. */
function bounds(p) {
  const total = p.getBoundingClientRect().width;
  const gap = splitter()?.getBoundingClientRect().width ?? 0;
  const max = Math.floor(Math.min(total * 0.65, total - MIN_PLAN - gap));
  return { min: MIN_BOARD, max: Math.max(MIN_BOARD, max) };
}
const clamp = (w, { min, max }) => Math.round(Math.min(max, Math.max(min, w)));

/** The separator's values, from the board as laid out. */
function frameSplitter() {
  const p = page(), s = splitter(), board = boardPane();
  if (!p || !s || !board || !s.checkVisibility?.()) return;
  const { min, max } = bounds(p);
  const now = Math.round(board.getBoundingClientRect().width);
  for (const [name, value] of [["aria-valuemin", min], ["aria-valuemax", max],
                               ["aria-valuenow", clamp(now, { min, max })],
                               ["aria-valuetext", `Board ${now} pixels wide`]]) {
    if (s.getAttribute(name) !== String(value)) s.setAttribute(name, value);
  }
}

function setWidth(p, width, keep) {
  if (width == null) {
    p.style.removeProperty("--board-w");
  } else {
    p.style.setProperty("--board-w", `${width}px`);
  }
  if (keep) store.set(widthKey(p.dataset.project), width == null ? null : String(width));
}

let restored = "";
function restoreWidth(p) {
  if (restored === p.dataset.project) return;
  restored = p.dataset.project;
  const saved = Number(store.get(widthKey(p.dataset.project)));
  // the page's grid keeps it within bounds when the window is narrower than when it was set
  if (saved >= MIN_BOARD) setWidth(p, Math.round(saved), false);
  requestAnimationFrame(frameSplitter);
}

let drag = null, frame = 0;
function endDrag() {
  if (!drag) return;
  cancelAnimationFrame(frame);
  frame = 0;
  if (drag.width != null) setWidth(drag.page, drag.width, true);
  drag = null;
  document.documentElement.classList.remove("resizing");
  window.dispatchEvent(new Event("sluice-resized"));  // the plan redraws its edges once
  frameSplitter();
}
document.addEventListener("pointerdown", event => {
  const s = event.target.closest?.(".splitter");
  if (!s || event.button !== 0) return;
  const p = s.closest("#project-board");
  event.preventDefault();
  s.focus({ preventScroll: true });
  s.setPointerCapture(event.pointerId);
  drag = { page: p, x: event.clientX, from: boardPane().getBoundingClientRect().width,
           bounds: bounds(p), width: null };
  document.documentElement.classList.add("resizing");
});
document.addEventListener("pointermove", event => {
  if (!drag) return;
  // the board is on the right: moving the splitter left widens it
  drag.width = clamp(drag.from - (event.clientX - drag.x), drag.bounds);
  if (frame) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    if (drag?.width != null) drag.page.style.setProperty("--board-w", `${drag.width}px`);
  });
});
for (const type of ["pointerup", "pointercancel", "lostpointercapture"]) {
  document.addEventListener(type, endDrag);
}
document.addEventListener("dblclick", event => {
  const s = event.target.closest?.(".splitter");
  if (!s) return;
  setWidth(s.closest("#project-board"), null, true);
  requestAnimationFrame(frameSplitter);
  window.dispatchEvent(new Event("sluice-resized"));
});
document.addEventListener("keydown", event => {
  const s = event.target.closest?.(".splitter");
  if (!s || event.altKey || event.ctrlKey || event.metaKey) return;
  const p = s.closest("#project-board"), b = bounds(p);
  const now = boardPane().getBoundingClientRect().width;
  const step = event.shiftKey ? BIG_STEP : STEP;
  const width = { ArrowLeft: now + step, ArrowRight: now - step, Home: b.min, End: b.max }[event.key];
  if (width == null) return;
  event.preventDefault();
  setWidth(p, clamp(width, b), true);
  requestAnimationFrame(frameSplitter);
  window.dispatchEvent(new Event("sluice-resized"));
});
let resizing = 0;
window.addEventListener("resize", () => {
  if (!resizing) resizing = requestAnimationFrame(() => { resizing = 0; frameSplitter(); });
});

// ---- the description's More -------------------------------------------------------------------

const aboutKey = id => `sluice.about.${id}`;
let aboutDone = "";
function restoreAbout(p) {
  if (aboutDone === p.dataset.project) return;
  aboutDone = p.dataset.project;
  const more = p.querySelector("details.about-more");
  if (more && store.get(aboutKey(p.dataset.project)) === "open") more.open = true;
}
document.addEventListener("toggle", event => {
  const more = event.target;
  if (!more.matches?.("details.about-more")) return;
  const p = more.closest("#project-board");
  store.set(aboutKey(p.dataset.project), more.open ? "open" : null);
}, true);

// ---- the board tools: apply on change, search as one types ------------------------------------

const tools = () => document.querySelector("#project-board form.board-tools");
let typing = 0;
function applyTools() {
  clearTimeout(typing);
  const form = tools(), p = page();
  if (!form || !p) return;
  const params = new URLSearchParams();
  const tag = new URLSearchParams(location.search).get("tag");
  for (const [name, value] of new FormData(form)) {
    const v = String(value).trim();
    if (name === "q" ? v : true) params.set(name, v);
  }
  if (tag) params.set("tag", tag);
  // the address leaves out the defaults; the stream asks for exactly what the page shows
  const shown = new URLSearchParams(params);
  if (shown.get("order") === "live") shown.delete("order");
  if (shown.get("show") === "all") shown.delete("show");
  const address = `${location.pathname}${shown.size ? `?${shown}` : ""}${location.hash}`;
  if (address !== `${location.pathname}${location.search}${location.hash}`) {
    history.replaceState(history.state, "", address);
  }
  restartStream(`${p.dataset.projectBase}/stream?${params}`);
}
/** Point the page's stream at `url`: Datastar ends the old request ('cleanup') and opens the
 * new one, whose first batch draws the board under the new query. */
function restartStream(url) {
  const main = document.querySelector("main[data-init]");
  const init = main?.getAttribute("data-init");
  if (!init) return;
  const next = init.replace(/@get\('[^']*'/, `@get('${url}'`);
  if (next === init) return;
  window.dispatchEvent(new Event("sluice-stream-restart"));
  main.setAttribute("data-init", next);
}

document.addEventListener("input", event => {
  if (!event.target.matches?.("form.board-tools input[name=q]")) return;
  clearTimeout(typing);
  typing = setTimeout(applyTools, 200);
});
document.addEventListener("change", event => {
  if (event.target.matches?.("form.board-tools select")) applyTools();
});
document.addEventListener("submit", event => {
  if (!event.target.matches?.("form.board-tools")) return;
  event.preventDefault();
  applyTools();
});
document.addEventListener("click", event => {
  const clear = event.target.closest?.("[data-clear-q]");
  if (!clear || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey) return;
  const input = tools()?.querySelector("input[name=q]");
  if (!input) return;
  event.preventDefault();
  input.value = "";
  input.focus();
  applyTools();
});
// Escape clears the search first (and only that, not also the open step)
window.addEventListener("keydown", event => {
  const input = event.target;
  if (event.key !== "Escape" || !input.matches?.("form.board-tools input[name=q]")) return;
  if (!input.value) return;
  event.preventDefault();
  event.stopPropagation();
  input.value = "";
  applyTools();
}, true);

// ---- a board Button: its say, sent without leaving the page ------------------------------------

document.addEventListener("submit", async event => {
  const form = event.target.closest?.("form.board-form");
  if (!form) return;
  event.preventDefault();
  const status = form.querySelector(".board-status");
  const buttons = [...form.querySelectorAll("button[name=button]")];
  for (const note of form.querySelectorAll("[data-error-for]")) {
    note.hidden = true;
    note.textContent = "";
  }
  const body = new URLSearchParams(new FormData(form, event.submitter));
  buttons.forEach(b => { b.disabled = true; });
  status.textContent = "Sending…";
  try {
    const response = await fetch(form.action, {
      method: "POST", headers: { accept: "application/json" }, body,
    });
    const got = await response.json().catch(() => ({}));
    if (response.ok) {
      status.textContent = got.message ?? "Sent.";
      return;
    }
    status.textContent = got.message ?? `Could not send (${response.status}).`;
    for (const raw of got.errors ?? []) {
      let error;
      try { error = JSON.parse(raw); } catch { continue; }
      const note = form.querySelector(`[data-error-for="${CSS.escape(error.field ?? "")}"]`);
      if (note) {
        note.textContent = error.message;
        note.hidden = false;
      }
    }
  } catch (error) {
    status.textContent = `Could not send: ${error.message}`;
  } finally {
    buttons.forEach(b => { b.disabled = false; });
  }
});

sync();
new MutationObserver(sync).observe(document.querySelector("#content") ?? document.body,
                                   { childList: true, subtree: true });
