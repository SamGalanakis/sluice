const runtimeUrl = document.querySelector("script[data-datastar-runtime]")?.src;
const datastar = runtimeUrl ? await import(runtimeUrl) : null;
// The nav's two menus (the project switcher, display preferences) and the plan's More menu are <details>: they open and
// work without this. This closes them on a click elsewhere or Escape, as a menu does, and makes
// a setting apply at once, without the menu's Save: the theme (its id as `data-theme` on
// <html>; until one is picked, or after "Match system", the attribute is absent and the page
// follows the OS) and value types (`show-types`, which the Types switch in a
// step's drawer also turns), each kept by posting it to /settings, which sets the cookie
// every page is rendered from.

const root = document.documentElement;
const menus = () => document.querySelectorAll("nav.top details[open], details.tool-more[open]");
document.addEventListener("click", (ev) => {
  for (const d of menus()) if (!d.contains(ev.target)) d.open = false;
});
document.addEventListener("keydown", (ev) => {
  if (ev.key !== "Escape") return;
  for (const d of menus()) {
    d.open = false;
    d.querySelector("summary").focus();
  }
});

function keep(name, value) {
  fetch("/settings", { method: "POST", keepalive: true,
                       body: new URLSearchParams({ [name]: value }) }).catch(() => {});
}

export const typesOn = () => root.classList.contains("show-types");

export function setTypes(on) {
  root.classList.toggle("show-types", on);
  for (const b of document.querySelectorAll(".types-toggle")) {
    b.setAttribute("aria-pressed", String(on));
  }
  for (const box of document.querySelectorAll("form.prefs input[name=types][type=checkbox]")) {
    box.checked = on;
  }
  keep("types", on ? "1" : "0");
}

const prefs = document.querySelector("form.prefs");
if (prefs) {
  prefs.querySelector(".save").hidden = true;
  prefs.addEventListener("change", (ev) => {
    const input = ev.target;
    if (input.name === "theme") {
      // "Match system" (no value) lets the page follow the OS again
      if (input.value) root.dataset.theme = input.value;
      else delete root.dataset.theme;
      keep("theme", input.value);
    } else if (input.name === "types") {
      setTypes(input.checked);
    }
  });
}

// the Types switch was kept in localStorage before it was a setting: carry it over once
try {
  if (localStorage.getItem("sluice.types") === "1" && !typesOn()) setTypes(true);
  localStorage.removeItem("sluice.types");
} catch { /* no storage */ }

// The stream uses Datastar's own retry and cancellation ownership. The status
// stays visible until the version marker acknowledges an entire rendered batch.
const streamState = document.querySelector("#stream-state");
// A page that points its stream at a new query (the board's search) ends the old request on
// purpose: its end is not a lost connection.
let restarts = 0;
window.addEventListener("sluice-stream-restart", () => { restarts++; });
document.addEventListener("datastar-fetch", (event) => {
  if (event.detail.el !== document.querySelector("main[data-init]")) return;
  if (restarts && event.detail.type === "finished") { restarts--; return; }
  if (["error", "retrying", "retries-failed", "finished"].includes(event.detail.type)) {
    datastar?.mergePatch({ stale: true });
  }
});
document.addEventListener("datastar-signal-patch", (event) => {
  if (event.detail.stale === false && streamState) streamState.style.display = "none";
  if (event.detail.stale === true && streamState) streamState.style.display = "";
  const title = document.querySelector("[data-page-title]");
  if (title) document.title = title.dataset.pageTitle;
  tick();
});
// A stream with no version changes still sends keepalives. Fetch failures are
// reported by Datastar, so an idle page is never marked stale by a timer.
document.querySelector(".stream-retry")?.addEventListener("click", () => location.reload());
// ---- times: one vocabulary on every page ------------------------------------------------------
// A <time> carries its instant in `datetime`; until this reads it, its text is the server's
// "2026-10-07 20:47 UTC" (and its title keeps that). `data-since`: how long since, ticking, in
// two units ("45s", "12m", "2h 14m", "3d 12h"); `data-ago`: "just now", "12m ago", "3d 12h ago".
// A card's timer (`.tk`, and `.vh` for a screen reader) holds its width, so only a longer text
// can move its card: then "sluice-resized" asks the board to redraw its edges. A stream patch
// writes the server's text back, so a patch is read again at once (the observer below).
export function short(seconds) {
  const s = Math.floor(Math.max(seconds, 0));
  const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60);
  if (s < 1) return "<1s";
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${m}m`;
  return s < 86400 ? `${h}h ${m}m` : `${d}d ${h}h`;
}
// the same in words, for a screen reader: "45 seconds", "2 hours 14 minutes", "1 day"
function spoken(seconds) {
  const s = Math.floor(Math.max(seconds, 0));
  const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60);
  const unit = (n, one) => (n === 0 ? "" : `${n} ${one}${n === 1 ? "" : "s"}`);
  if (s < 1) return "under a second";
  const parts = s < 60 ? [unit(s, "second")] : s < 3600 ? [unit(m, "minute")]
    : s < 86400 ? [unit(h, "hour"), unit(m, "minute")] : [unit(d, "day"), unit(h, "hour")];
  return parts.filter(Boolean).join(" ");
}
const ago = (seconds) => (seconds < 60 ? "just now" : `${short(seconds)} ago`);
const QUIET = 2 * 3600;  // the default seconds without a write before a running step is quiet (data-quiet-after)

function tick() {
  const now = Date.now();
  let wider = false;
  for (const t of document.querySelectorAll("time[datetime]")) {
    const seconds = (now - Date.parse(t.dateTime)) / 1000;
    if (!Number.isFinite(seconds)) continue;
    if (t.hasAttribute("data-since")) {
      const shown = t.querySelector(":scope > .tk"), said = t.querySelector(":scope > .vh");
      const text = short(seconds);
      if (!shown) {
        if (t.textContent !== text) t.textContent = text;
        continue;
      }
      if (shown.textContent !== text) {
        wider ||= text.length > shown.textContent.length;
        shown.textContent = text;
      }
      const words = ` for ${spoken(seconds)}`;
      if (said && said.textContent !== words) said.textContent = words;
    } else {
      const text = ago(seconds);
      if (t.textContent !== text) t.textContent = text;
    }
  }
  if (wider) window.dispatchEvent(new Event("sluice-resized"));
  // a running step on the index gone quiet: "quiet" when it has written nothing since it
  // started, else "quiet 42m"
  let quiet = 0;
  for (const tag of document.querySelectorAll("[data-quiet-since]")) {
    const age = now / 1000 - Number(tag.dataset.quietSince);
    const ran = (now - Date.parse(tag.dataset.runSince)) / 1000;
    tag.hidden = !(age >= Number(tag.dataset.quietAfter || QUIET));
    if (tag.hidden) continue;
    const text = Math.abs(ran - age) < 60 ? "quiet" : `quiet ${short(age)}`;
    const words = tag.querySelector(".qt") ?? tag;  // the hourglass stays
    if (words.textContent !== text) words.textContent = text;
    if (!tag.closest("details.archived")) quiet++;
  }
  const title = document.querySelector("[data-page-title]");
  if (title) title.dataset.pageTitle = title.dataset.pageTitle.replace(/(?:\d+ quiet · )?Projects/, `${quiet ? quiet + " quiet · " : ""}Projects`);
  if (title && document.title !== title.dataset.pageTitle) document.title = title.dataset.pageTitle;
}
tick();
setInterval(tick, 5000);
new MutationObserver(tick).observe(document.querySelector("main") ?? document.body,
  { childList: true, subtree: true, characterData: true });
