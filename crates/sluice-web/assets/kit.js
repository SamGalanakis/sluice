// The kit's behaviour (views::ui; DESIGN.md, Components): tabs, deep links into a tab, the
// message box and the confirmation dialog. Every page loads it; each part works without it
// (tabs fall back to their panels stacked under their heads, the message box to a form post,
// a confirmation to its form inline). Adapted, not depended on: the tabs' keyboard model is
// github/tab-container-element's and basecoat's (MIT), the look oat's and basecoat's.

// ---- tabs ----------------------------------------------------------------------------------
// A tab set is `[data-tabs]`: a `[role=tablist]` of `[role=tab][data-tab]` buttons and its
// `.tp[data-tab]` panels. The chosen tab is the set's `data-current`; its panel carries
// `data-chosen` (the stylesheet hides the rest while script runs). A stream patch never resets any
// of these (`data-preserve-attr`), and a patch that draws a new tab or panel is brought in line
// at once (the observer below, before the next frame). The choice is also the page's `tab`
// signal, which the step's stream reads when it connects, and, on a step's own page
// (`data-tab-url`), the address's `?tab=` (replaceState: no history entry per click).
let mergePatch = null;
const runtime = document.querySelector("script[data-datastar-runtime]")?.src;
if (runtime) import(runtime).then((m) => { mergePatch = m.mergePatch; }).catch(() => {});

const tabsOf = (set) => [...set.querySelectorAll(":scope > .tabbar [role=tab]")];
const panelsOf = (set) => [...set.querySelectorAll(":scope > .tp")];

function apply(set, key) {
  const tabs = tabsOf(set);
  if (!tabs.some((t) => t.dataset.tab === key)) key = tabs[0]?.dataset.tab ?? "";
  if (set.dataset.current !== key) set.dataset.current = key;
  for (const tab of tabs) {
    const on = tab.dataset.tab === key;
    if (tab.getAttribute("aria-selected") !== String(on)) tab.setAttribute("aria-selected", String(on));
    const index = on ? "0" : "-1";
    if (tab.getAttribute("tabindex") !== index) tab.setAttribute("tabindex", index);
  }
  for (const panel of panelsOf(set)) {
    const on = panel.dataset.tab === key;
    if (panel.hasAttribute("data-chosen") !== on) panel.toggleAttribute("data-chosen", on);
    if (panel.getAttribute("tabindex") !== "0") panel.setAttribute("tabindex", "0");
  }
  overflow(set);
  return key;
}
// a bar wider than its room says so: it fades out at its end until scrolled there
function overflow(set) {
  const bar = set.querySelector(":scope > .tabbar > .tablist");
  if (!bar) return;
  const more = bar.scrollLeft + bar.clientWidth < bar.scrollWidth - 2;
  if (bar.hasAttribute("data-more") !== more) bar.toggleAttribute("data-more", more);
}
document.addEventListener("scroll", (event) => {
  if (event.target.matches?.(".tablist")) overflow(event.target.closest("[data-tabs]"));
}, true);
window.addEventListener("resize", () => document.querySelectorAll("[data-tabs]").forEach(overflow));

/** Choose a tab: its panel shows, the signal and (on a step's page) the address follow. */
export function select(set, key, { focus = false } = {}) {
  key = apply(set, key);
  const chosen = tabsOf(set).find((t) => t.dataset.tab === key);
  chosen?.scrollIntoView({ block: "nearest", inline: "nearest" });
  if (focus) chosen?.focus();
  try { mergePatch?.({ tab: key }); } catch { /* no signals on this page */ }
  if (set.hasAttribute("data-tab-url")) {
    const url = new URL(location.href);
    if (key === tabsOf(set)[0]?.dataset.tab) url.searchParams.delete("tab");
    else url.searchParams.set("tab", key);
    if (url.href !== location.href) history.replaceState(history.state, "", url);
  }
}

document.addEventListener("click", (event) => {
  const tab = event.target.closest?.("[data-tabs] > .tabbar [role=tab]");
  if (tab) select(tab.closest("[data-tabs]"), tab.dataset.tab);
});
// arrows move along the bar (wrapping), Home and End to its ends; each move chooses the tab
document.addEventListener("keydown", (event) => {
  const tab = event.target.closest?.("[data-tabs] > .tabbar [role=tab]");
  if (!tab) return;
  const set = tab.closest("[data-tabs]");
  const tabs = tabsOf(set);
  const at = tabs.indexOf(tab);
  const next = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: tabs.length - 1 }[event.key];
  if (next === undefined) return;
  event.preventDefault();
  select(set, tabs[(next + tabs.length) % tabs.length].dataset.tab, { focus: true });
});

// ---- deep links: an anchor in a panel opens its tab ---------------------------------------
// `#run-3`, a failing call's anchor, `#message-12`, `#activity`: the tab holding the target is
// chosen, a fold around it opened, and it is brought into view. In the drawer the address's
// hash is the open step's (`#step:<id>`), so a link there never changes it.
function reveal(id, { scroll = true } = {}) {
  let target = null;
  try { target = id && document.getElementById(decodeURIComponent(id)); } catch { return false; }
  const panel = target?.closest(".tp");
  const set = panel?.closest("[data-tabs]");
  if (!set) return false;
  if (set.dataset.current !== panel.dataset.tab) select(set, panel.dataset.tab);
  for (let d = target.closest("details:not([open])"); d; d = d.parentElement?.closest("details:not([open])")) d.open = true;
  if (target.matches("details:not([open])")) target.open = true;
  if (scroll) target.scrollIntoView({ block: "start" });
  return true;
}
document.addEventListener("click", (event) => {
  const a = event.target.closest?.("a[href^='#'], a[data-tab-to]");
  if (!a || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  const set = a.closest("[data-tabs]");
  if (a.dataset.tabTo && set && !a.getAttribute("href")?.startsWith("#")) {
    event.preventDefault();
    select(set, a.dataset.tabTo);
    return;
  }
  const id = a.getAttribute("href").slice(1);
  if (!id || id.startsWith("step:")) return;
  const inDrawer = Boolean(a.closest(".drawer"));
  // the panel shows before the browser follows the link, so it scrolls to a target it can see
  if (reveal(id, { scroll: inDrawer }) && inDrawer) event.preventDefault();
});
const fromHash = () => {
  const id = location.hash.slice(1);
  if (id && !id.startsWith("step:")) reveal(id);
};
window.addEventListener("hashchange", fromHash);

// ---- the message box: a note or a question to its step or the orchestrator -----------------
// It posts as JSON and stays on the page: the conversation's stream brings the message in, the
// box keeps what is typed in it until then (`data-ignore-morph`).
const unreached = (error) =>
  error instanceof TypeError ? "Sluice did not answer: is it running? Try again in a moment." : error.message;
document.addEventListener("submit", async (event) => {
  const form = event.target;
  if (!form.matches?.("form.composer")) return;
  event.preventDefault();
  const status = form.querySelector(".ou-status");
  const button = form.querySelector("button");
  const data = new FormData(form);
  button.disabled = true;
  try {
    const response = await fetch(form.action, { method: "POST", headers: { "content-type": "application/json" },
                                                 body: JSON.stringify({ body: data.get("body"), to: data.get("to"), ask: data.get("ask") === "true" }) });
    const result = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(result.message ?? "Sluice did not take the message.");
    status.textContent = "Sent.";
    form.reset();
  } catch (error) {
    status.textContent = unreached(error);
  } finally {
    button.disabled = false;
  }
});

// ---- the confirmation dialog ----------------------------------------------------------------
// A confirmation (Cancel, Close all, Delete) stays the <details class="confirm-flow"> the server
// draws (`ui::Confirm`), so a patch of its region morphs it in place: its summary keeps the focus
// and any click that straddles the patch. With script, the summary opens the shared dialog
// instead of the details, the form moved into it while it is open; without, the details open
// inline.
const confirmation = document.querySelector("#confirmation");
let confirming;
document.addEventListener("click", (event) => {
  const summary = event.target.closest?.("details.confirm-flow > summary");
  if (!summary || !confirmation) return;
  event.preventDefault();
  if (summary.getAttribute("aria-disabled") === "true" || summary.hasAttribute("data-disabled") || confirmation.open) return;
  const flow = summary.parentElement;
  const form = flow.querySelector(":scope > form");
  if (!form) return;
  const place = document.createComment("confirmation form");
  form.replaceWith(place);
  confirming = { form, place, flow, summary, title: flow.dataset.confirmTitle };
  // a step named by its title, its id after it in data mono: "Cancel L13: certif… fig-5193-work?"
  const head = document.querySelector("#confirmation-title");
  head.textContent = flow.dataset.confirmTitle;
  if (flow.dataset.confirmId) {
    const id = Object.assign(document.createElement("code"), { className: "sref-id", textContent: flow.dataset.confirmId });
    head.append(" ", id, "?");
  }
  document.querySelector("#confirmation-body").replaceChildren(form);
  form.querySelector(".confirm-copy").id = "confirmation-copy";
  confirmation.setAttribute("aria-describedby", "confirmation-copy");
  confirmation.showModal();
  form.querySelector("[data-keep]").focus();
});
confirmation?.addEventListener("close", () => {
  if (!confirming) return;
  const { form, place, flow, summary, title } = confirming;
  confirming = null;
  form.querySelector(".confirm-copy").removeAttribute("id");
  // a patch while it was open may have drawn the details a new form: keep that one
  if (place.isConnected && !flow.querySelector(":scope > form")) place.replaceWith(form);
  else { place.remove(); form.remove(); }
  const opener = summary.isConnected ? summary
    : [...document.querySelectorAll("details.confirm-flow")].find((f) => f.dataset.confirmTitle === title)?.querySelector(":scope > summary");
  opener?.focus();
});
document.addEventListener("click", (event) => {
  const keep = event.target.closest?.("[data-keep]");
  if (!keep) return;
  if (confirmation?.open && confirmation.contains(keep)) confirmation.close();
  else keep.closest("details.confirm-flow").open = false;
});
document.addEventListener("keydown", (event) => {
  if (event.key !== "Tab" || !confirmation?.open) return;
  const controls = [...confirmation.querySelectorAll("button:not(:disabled), textarea, input:not([type=hidden])")];
  const first = controls[0], last = controls.at(-1);
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
});

// ---- every tab set in line, now and after each patch ---------------------------------------
function settle() {
  for (const set of document.querySelectorAll("[data-tabs]")) apply(set, set.dataset.current);
}
settle();
fromHash();
// the bar's room is known once its words are drawn in their font
document.fonts?.ready.then(() => document.querySelectorAll("[data-tabs]").forEach(overflow));
window.addEventListener("load", () => document.querySelectorAll("[data-tabs]").forEach(overflow));
new MutationObserver(settle).observe(document.body, { childList: true, subtree: true });
