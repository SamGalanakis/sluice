const runtimeUrl = document.querySelector("script[data-datastar-runtime]")?.src;
const datastar = runtimeUrl ? await import(runtimeUrl) : null;
// The nav's two menus, the project switcher and the settings cog, are <details>: they open and
// work without this. This closes them on a click elsewhere or Escape, as a menu does, and makes
// a setting apply at once, without the menu's Save: the theme (its id as `data-theme` on
// <html>; until one is picked the attribute is absent, the page follows the OS and the menu
// marks the matching preset) and value types (`show-types`, which the Types switch in a
// step's drawer also turns), each kept by posting it to /settings, which sets the cookie
// every page is rendered from.

const root = document.documentElement;
const menus = () => document.querySelectorAll("nav.top details[open]");
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
  // nothing picked yet: the page follows the OS, so the menu shows that theme as chosen
  if (!root.dataset.theme) {
    const os = matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
    const radio = prefs.querySelector(`input[name=theme][value=${os}]`);
    if (radio) radio.checked = true;
  }
  prefs.addEventListener("change", (ev) => {
    const input = ev.target;
    if (input.name === "theme") {
      root.dataset.theme = input.value;
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
document.addEventListener("datastar-fetch", (event) => {
  if (event.detail.el !== document.querySelector("main[data-init]")) return;
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
function duration(seconds) {
  const minutes = Math.floor(Math.max(0, seconds) / 60);
  return minutes >= 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m` : `${minutes}m`;
}
function tick() {
  for (const element of document.querySelectorAll("[data-started]")) {
    const time = Date.parse(element.dataset.started);
    if (Number.isFinite(time)) element.textContent = duration((Date.now() - time) / 1000);
  }
  for (const element of document.querySelectorAll("time[datetime]")) {
    const time = Date.parse(element.dateTime);
    if (Number.isFinite(time)) element.textContent = `${duration((Date.now() - time) / 1000)} ago`;
  }
  let quiet = 0;
  for (const element of document.querySelectorAll("[data-quiet-since]")) {
    const seconds = Date.now() / 1000 - Number(element.dataset.quietSince);
    element.hidden = seconds < 900;
    if (!element.hidden) { element.textContent = `quiet ${duration(seconds)}`; if (!element.closest("details.archived")) quiet++; }
  }
  const title = document.querySelector("[data-page-title]");
  if (title) title.dataset.pageTitle = title.dataset.pageTitle.replace(/(?:\d+ quiet · )?Projects/, `${quiet ? quiet + " quiet · " : ""}Projects`);
  if (title && document.title !== title.dataset.pageTitle) document.title = title.dataset.pageTitle;
}
tick();
setInterval(tick, 10000);
