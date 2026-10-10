// The project page with its board beside the plan (docs("board")). Without script both
// sections show (side by side from 1280px, one after the other below), the board's tools apply
// with their Apply button and a Button posts the page. With script (its search is
// `sluice-search`, the splitter `sluice-splitter`, the description's More and the board's
// document `sluice-fold`: components.js):
// - the view switch: Plan · Both · Board from 1280px, Plan · Board below; each remembered per
//   project (localStorage), wide and narrow apart; until one is picked a phone shows the board
//   (its live lanes are the quick check) unless the project needs attention (a failure, a
//   cancel, a quiet run or a pause: `data-attention`), when it shows the plan, which leads
//   with what stopped; the window between a phone and 1280px shows the plan;
// - the board column's height and its "More below" cue;
// - a board Button's say is sent without leaving the page, the answer shown under the board;
// - a step whose state the live plan moves on is said once, politely, in #announce.
const WIDE = matchMedia("(min-width: 1280px)");
const PHONE = matchMedia("(max-width: 720px)");  // a phone opens on the board, unless something needs attention
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
// an address that asks for a search, a Show, an Order, a step's chain or a recipe's units is
// about the plan
const planAsked = () => {
  const q = new URLSearchParams(location.search);
  return ["q", "show", "order", "root", "recipe"].some(k => q.get(k));
};
function viewOf(p) {
  if (!p.classList.contains("has-panel")) return "plan";
  const m = mode();
  const views = m === "wide" ? ["plan", "both", "board"] : ["plan", "board"];
  const v = chosen[m] ?? store.get(viewKey(m, p.dataset.project));
  if (!(m in chosen) && planAsked()) return m === "wide" && v === "plan" ? "plan" : m === "wide" ? "both" : "plan";
  if (views.includes(v)) return v;
  if (m === "wide") return "both";
  return PHONE.matches && !p.hasAttribute("data-attention") ? "board" : "plan";
}

/** Put the page in its view and the switch in step with it. */
function sync() {
  const p = page();
  if (!p) return;
  const view = viewOf(p);
  if (p.dataset.view !== view) p.dataset.view = view;
  // the switch is in the page's row, under the band
  for (const tab of document.querySelectorAll("[data-view-tab]")) {
    const on = String(tab.dataset.viewTab === view);
    if (tab.getAttribute("aria-pressed") !== on) tab.setAttribute("aria-pressed", on);
  }
}
document.addEventListener("click", event => {
  const tab = event.target.closest?.("[data-view-tab]");
  const p = tab && page();
  if (!p) return;
  chosen[mode()] = tab.dataset.viewTab;
  store.set(viewKey(mode(), p.dataset.project), tab.dataset.viewTab);
  sync();
});
WIDE.addEventListener("change", sync);
PHONE.addEventListener("change", sync);

// ---- the board column ------------------------------------------------------------------------

const boardPane = () => document.querySelector("#board-pane");
let resizing = 0;
window.addEventListener("resize", () => {
  if (!resizing) resizing = requestAnimationFrame(() => { resizing = 0; fitPane(); });
});
// The board column scrolls on its own and is at most the window's height once it sticks; until
// the page has scrolled it up to its sticky top, it ends where the window does, so its own end
// (and the shade that says more lies past it) is always in view.
function fitPane() {
  const pane = boardPane();
  if (!pane) return;
  const top = Math.max(16, pane.getBoundingClientRect().top);
  const height = `${Math.max(240, Math.floor(window.innerHeight - top - 16))}px`;
  if (pane.style.getPropertyValue("--pane-max") !== height) pane.style.setProperty("--pane-max", height);
  moreBelow();
}
// While the column scrolls on its own and more of it lies below, a "More below" cue sits at its
// foot over the shade; it scrolls the column on by most of a screen, and goes at the end.
function moreBelow() {
  const pane = boardPane(), cue = pane?.querySelector(".more-below");
  if (!cue) return;
  const scrolls = getComputedStyle(pane).overflowY === "auto";
  const hide = !scrolls || pane.scrollTop + pane.clientHeight >= pane.scrollHeight - 24;
  if (cue.hidden !== hide) cue.hidden = hide;
}
document.addEventListener("scroll", (event) => {
  if (event.target === boardPane()) moreBelow();
}, { capture: true, passive: true });
document.addEventListener("click", (event) => {
  const cue = event.target.closest?.(".more-below");
  const pane = cue?.closest("#board-pane");
  if (pane) pane.scrollBy({ top: pane.clientHeight * 0.8, behavior: "smooth" });
});
let fitting = 0;
window.addEventListener("scroll", () => {
  if (!fitting) fitting = requestAnimationFrame(() => { fitting = 0; fitPane(); });
}, { passive: true });
// a patch may redraw the column without the height it was given
new MutationObserver(() => {
  if (!fitting) fitting = requestAnimationFrame(() => { fitting = 0; fitPane(); });
}).observe(document.querySelector("main") ?? document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["style"] });
fitPane();

// ---- a board Button: its say, sent without leaving the page ------------------------------------

// A refused field's note stays through the board's patches (`data-ignore-morph`) until the owner
// edits that field, or sends the form again.
const clearNote = event => {
  const field = event.target.closest?.("form.board-form [name]");
  const note = field && field.form?.querySelector(`[data-error-for="${CSS.escape(field.name)}"]`);
  if (note && !note.hidden) {
    note.hidden = true;
    note.textContent = "";
  }
};
document.addEventListener("input", clearNote);
document.addEventListener("change", clearNote);

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
    status.textContent = got.message ?? `Not sent: sluice refused it (${response.status}).`;
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
    status.textContent = error instanceof TypeError
      ? "Not sent: sluice did not answer. Is it running? Try again in a moment."
      : `Not sent: ${error.message}`;
  } finally {
    buttons.forEach(b => { b.disabled = false; });
  }
});

// ---- what the live plan moved on, said once ---------------------------------------------------
// Each step's state in words: a stage cell's (its link's data-step, its hidden ": word"), and a
// stopped module's or a Done line's data-said ("a-build succeeded|a-review skipped"), so a step
// is still read when its unit changes band.
function states() {
  const out = new Map();
  const plan = document.querySelector("#plan-pane");
  if (!plan) return out;
  for (const a of plan.querySelectorAll("li.sc[data-state] > a.sc-a[data-step]")) {
    const word = a.querySelector(":scope > .vh")?.textContent.replace(/^:\s*/, "").split(",")[0].trim();
    if (word) out.set(a.dataset.step, word);
  }
  for (const el of plan.querySelectorAll("[data-said]")) {
    for (const pair of el.dataset.said.split("|")) {
      const at = pair.indexOf(" ");
      if (at > 0 && !out.has(pair.slice(0, at))) out.set(pair.slice(0, at), pair.slice(at + 1));
    }
  }
  return out;
}
let known = states(), looking = 0;
function said() {
  looking = 0;
  const now = states(), moved = [];
  for (const [step, word] of now) {
    const was = known.get(step);
    if (was && was !== word) moved.push(`${step} ${word}`);
  }
  known = now;
  const live = document.getElementById("announce");
  if (!live || !moved.length) return;
  live.textContent = "";
  requestAnimationFrame(() => { live.textContent = moved.join(". "); });
}
new MutationObserver(() => { looking ||= requestAnimationFrame(said); })
  .observe(document.querySelector("#content") ?? document.body,
           { childList: true, subtree: true, attributes: true, attributeFilter: ["data-state", "data-said"] });

sync();
new MutationObserver(sync).observe(document.querySelector("#content") ?? document.body,
                                   { childList: true, subtree: true });
