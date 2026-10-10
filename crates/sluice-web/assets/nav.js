// Every page's clock and tab title (the dashboard's components are components.js's), and the
// theme a first open keeps.

// ---- the theme on a first open -----------------------------------------------------------------
// With no theme chosen yet (no `sluice_theme` cookie, so <html> carries no data-theme) the page
// is Sluice in the system's scheme, by the stylesheet alone. The first open with script keeps
// that as the choice (Sluice Light or Sluice Dark, by `prefers-color-scheme` now), so from then
// on only the owner's own pick changes the theme, never a later change of the system's scheme.
{
  const root = document.documentElement;
  if (!root.dataset.theme) {
    const theme = matchMedia("(prefers-color-scheme: dark)").matches ? "sluice-dark" : "sluice-light";
    root.dataset.theme = theme;
    for (const radio of document.querySelectorAll("input[name=theme]")) radio.checked = radio.value === theme;
    fetch("/settings", { method: "POST", keepalive: true, body: new URLSearchParams({ theme }) }).catch(() => {});
  }
}
document.addEventListener("datastar-signal-patch", () => {
  const title = document.querySelector("[data-page-title]");
  if (title) document.title = title.dataset.pageTitle;
  tick();
});
// A hidden tab's stream is closed (Datastar reopens it when the tab shows again), but its title
// is what the owner reads in the tab strip: while hidden it asks for the title alone every 30 s
// (a timer, not a frame: a hidden tab runs no animation frames).
let titlePoll = 0;
async function pollTitle() {
  const el = document.querySelector("[data-page-title][data-title-src]");
  if (!el) return;
  try {
    const response = await fetch(el.dataset.titleSrc, { headers: { accept: "text/plain" } });
    const text = response.ok ? (await response.text()).trim() : "";
    if (text) { el.dataset.pageTitle = text; document.title = text; }
  } catch { /* the next poll, or the stream when the tab shows again */ }
}
document.addEventListener("visibilitychange", () => {
  clearInterval(titlePoll);
  titlePoll = document.hidden && document.querySelector("[data-title-src]") ? setInterval(pollTitle, 30000) : 0;
});
// ---- times: one vocabulary on every page ------------------------------------------------------
// A <time> carries its instant in `datetime`; the server draws its text as it read the clock at
// render (so a page without script says "running for 1h 25m"), its title the UTC day and minute.
// This ticks it on in the same words. `data-since`: how long since, in two units ("45s", "12m",
// "2h 14m", "3d 12h"); `data-ago`: "just now", "12m ago", "3d 12h ago". The text is the clock's:
// a stream's version leaves it out, so ticking never patches.
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
    if (t.hasAttribute("data-clock")) {
      // a time of day on the reader's own clock: "17:49" today, "Oct 8 17:49" before
      const at = new Date(t.dateTime), today = new Date();
      const hm = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
      const text = at.toDateString() === today.toDateString() ? hm
        : `${at.toLocaleDateString([], { month: "short", day: "numeric" })} ${hm}`;
      if (t.textContent !== text) t.textContent = text;
      continue;
    }
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
      // one span for a screen reader, so it hears one phrase: what follows the time (", usually
      // 22 minutes") is kept in data-tail
      const words = ` for ${spoken(seconds)}${said?.dataset.tail ?? ""}`;
      if (said && said.textContent !== words) said.textContent = words;
      // a running card whose stage usually takes so long: how far along it is, a faint line
      // along its foot (full once past it); never the quiet gold
      const usually = Number(t.dataset.usually);
      if (usually > 0) {
        const along = String(Math.min(1, seconds / usually).toFixed(3));
        const card = t.closest(".node");
        if (card && card.style.getPropertyValue("--along") !== along) card.style.setProperty("--along", along);
      }
    } else {
      const text = ago(seconds);
      if (t.textContent !== text) t.textContent = text;
    }
  }
  if (wider) window.dispatchEvent(new Event("sluice-resized"));
  dayLine(now);
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
// ---- the day (/day): its hours on the reader's clock, its rule at now -------------------------
// The server lays the day's hours out on the reader's clock from this cookie: minutes east of
// UTC (UTC until it is set, so the page's stream redraws it in local hours at once).
{
  const zone = String(-new Date().getTimezoneOffset());
  if (!document.cookie.split("; ").includes(`sluice_zone=${zone}`)) {
    document.cookie = `sluice_zone=${zone}; path=/; max-age=34560000; samesite=lax`;
  }
}
// The day line's rule at now, and each running run's bar to it: the server draws both as it
// read the clock; this moves them on (the stream's version leaves the clock out).
function dayLine(now) {
  for (const line of document.querySelectorAll(".dl[data-from][data-to]")) {
    const from = Number(line.dataset.from) * 1000, to = Number(line.dataset.to) * 1000;
    if (!(to > from)) continue;
    const at = (t) => Math.min(100, Math.max(0, (t - from) / (to - from) * 100));
    const here = `${at(now).toFixed(3)}%`;
    if (line.style.getPropertyValue("--now") !== here) line.style.setProperty("--now", here);
    for (const bar of line.querySelectorAll(".dl-bar[data-open]")) {
      const width = `${Math.max(0.15, at(now) - at(Number(bar.dataset.open) * 1000)).toFixed(3)}%`;
      if (bar.style.width !== width) bar.style.width = width;
    }
  }
}
tick();
setInterval(tick, 5000);
new MutationObserver(tick).observe(document.querySelector("main") ?? document.body,
  { childList: true, subtree: true, characterData: true });

// An action that comes back to a place on its page (Dismiss to its Undo, Undo to the card it
// restored) names it in the address's fragment: the focus goes there, not to the page's top.
{
  const id = decodeURIComponent(location.hash.slice(1));
  const target = id ? document.getElementById(id) : null;
  if (target?.matches("button, [tabindex='-1']")) target.focus();
}
