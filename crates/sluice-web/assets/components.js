// The dashboard's components (DESIGN.md, Components). Rust draws each one's host and all that is
// in it (`views::ui::rocket`); each is a Rocket component in light DOM whose `setup` only
// enhances what the server drew: listeners, observers, keyboard handling and state kept in the
// host's own attributes, which a stream patch leaves as they are (`data-preserve-attr`). None
// draws content of its own, so a page reads whole without script, a patch morphs server HTML
// into server HTML, and a screen reader hears what the server wrote. The only text written here
// is chrome with no meaning without script (a status after a send, "Copied").
//
// The board and the drawer are `sluice.js`'s; the clock and the tab title `nav.js`'s. Adapted,
// not depended on: the tabs' keyboard model is github/tab-container-element's and basecoat's
// (MIT), the look oat's and basecoat's (assets/README.md).
const runtime = document.querySelector("script[data-datastar-runtime]")?.src;
const { rocket, mergePatch } = await import(runtime);

const root = document.documentElement;
const define = (tag, spec) => rocket(tag, { mode: "light", renderOnPropChange: false, ...spec });
/** Listeners a setup adds, all removed by its cleanup. */
function listening(cleanup) {
  const done = new AbortController();
  cleanup(() => done.abort());
  return (target, type, fn, options = {}) => target.addEventListener(type, fn, { ...options, signal: done.signal });
}
const plainClick = (event) => event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey;
const reduced = matchMedia("(prefers-reduced-motion: reduce)");
const smooth = () => (reduced.matches ? "auto" : "smooth");
// a request that never reached sluice reads in our words, not the browser's ("Failed to fetch")
const unreached = (error) =>
  error instanceof TypeError ? "Sluice did not answer: is it running? Try again in a moment." : error.message;
const store = {
  get(key) { try { return localStorage.getItem(key); } catch { return null; } },
  set(key, value) {
    try { if (value == null) localStorage.removeItem(key); else localStorage.setItem(key, value); } catch { /* not kept */ }
  },
};

// Owner action forms include ones moved into the shared confirmation dialog. Delegate on
// the document so streamed replacements and moved forms keep the same enhancement.
const postingActions = new WeakSet();
document.addEventListener("submit", async (event) => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement) || form.method !== "post") return;
  const url = new URL(form.getAttribute("action"), location.href);
  if (url.origin !== location.origin || !(/^\/projects\/id\/[^/]+\/steps\/[^/]+\/actions$/.test(url.pathname)
      || form.matches(".pl-close"))) return;
  event.preventDefault();
  if (postingActions.has(form)) return;
  postingActions.add(form);
  const data = new FormData(form, event.submitter);
  const button = event.submitter ?? form.querySelector("button");
  const dialog = form.closest("dialog");
  const next = new URL(data.get("next") || location.href, location.href);
  const focusId = next.pathname === location.pathname
    && /^(undo-|s-)/.test(next.hash.slice(1)) ? decodeURIComponent(next.hash.slice(1)) : "";
  const scroll = { left: scrollX, top: scrollY };
  let observer, timer;
  const stop = () => { observer?.disconnect(); clearTimeout(timer); };
  if (focusId) {
    observer = new MutationObserver(() => {
      const target = document.getElementById(focusId);
      if (!target) return;
      stop();
      target.focus({ preventScroll: true });
      window.scrollTo(scroll);
    });
    observer.observe(document.getElementById("project-board") ?? document.body, { childList: true, subtree: true });
    timer = setTimeout(stop, 10000);
  }
  form.querySelector(".action-notice")?.remove();
  if (button) button.disabled = true;
  try {
    const response = await fetch(url, { method: "POST", headers: { accept: "application/json" }, redirect: "manual", body: new URLSearchParams(data) });
    if (!response.ok && response.type !== "opaqueredirect") {
      const text = await response.text();
      let message = text;
      try { message = JSON.parse(text).message ?? text; } catch { /* a text refusal */ }
      throw new Error(message || "Sluice did not take the action.");
    }
    if (dialog?.open) dialog.close();
  } catch (error) {
    stop();
    const notice = Object.assign(document.createElement("div"), { className: "notice action-notice", role: "alert" });
    notice.append(Object.assign(document.createElement("p"), { textContent: unreached(error) }));
    form.append(notice);
  } finally {
    if (button) button.disabled = false;
    postingActions.delete(form);
  }
});

// ---- sluice-tabs -------------------------------------------------------------------------------
// An ARIA tablist (`[role=tab][data-tab]` in its `.tabbar`) over its `.tp[data-tab]` panels. The
// choice is `$$tab`, reflected into the host's `current` (kept through a patch), the chosen
// tab's aria-selected and tabindex and its panel's `data-chosen` (the stylesheet hides the rest
// while script runs); also the page's `tab` signal, which the step's stream reads when it
// connects, and with `url` the address's `?tab=` (replaceState: no history entry per click). A
// patch that draws a new tab or panel is brought in line at once. An anchor to something in a
// panel (`#run-3`, a failing call, `#message-12`, `#activity`) opens its tab on it; in the
// drawer, whose hash is the open step's (`#step:<id>`), such a link never changes the address.
define("sluice-tabs", {
  props: ({ string, bool }) => ({ current: string, url: bool }),
  manifest: {
    slots: [{ name: "tabbar", description: "The server's .tabbar with its [role=tablist] of [role=tab][data-tab] buttons." },
            { name: "panels", description: "Its .tp[data-tab] sections, each under its own head (stacked without script)." }],
    events: [{ name: "sluice-tab", description: "A tab was chosen; detail.key is its key." }],
  },
  setup({ host, props, $$, effect, observeProps, cleanup }) {
    const on = listening(cleanup);
    const tabs = () => [...host.querySelectorAll(":scope > .tabbar [role=tab]")];
    const panels = () => [...host.querySelectorAll(":scope > .tp")];
    const valid = (key) => (tabs().some((t) => t.dataset.tab === key) ? key : tabs()[0]?.dataset.tab ?? "");
    const bar = () => host.querySelector(":scope > .tabbar > .tablist");
    // a bar wider than its room says so: it fades out at its end until scrolled there
    const overflow = () => {
      const list = bar();
      if (!list) return;
      const more = list.scrollLeft + list.clientWidth < list.scrollWidth - 2;
      if (list.hasAttribute("data-more") !== more) list.toggleAttribute("data-more", more);
    };
    const apply = (key) => {
      key = valid(key);
      for (const tab of tabs()) {
        const chosen = tab.dataset.tab === key;
        if (tab.getAttribute("aria-selected") !== String(chosen)) tab.setAttribute("aria-selected", String(chosen));
        const index = chosen ? "0" : "-1";
        if (tab.getAttribute("tabindex") !== index) tab.setAttribute("tabindex", index);
      }
      for (const panel of panels()) {
        const chosen = panel.dataset.tab === key;
        if (panel.hasAttribute("data-chosen") !== chosen) panel.toggleAttribute("data-chosen", chosen);
        if (panel.getAttribute("tabindex") !== "0") panel.setAttribute("tabindex", "0");
      }
      if (props.current !== key) host.current = key;
      overflow();
      return key;
    };
    $$("tab", valid(props.current));
    effect(() => apply($$.tab));
    observeProps(({ current }) => { if (current !== $$.tab) $$.tab = valid(current); }, "current");

    const choose = (key, { focus = false } = {}) => {
      key = valid(key);
      $$.tab = key;
      apply(key);
      const tab = tabs().find((t) => t.dataset.tab === key);
      tab?.scrollIntoView({ block: "nearest", inline: "nearest" });
      if (focus) tab?.focus();
      try { mergePatch({ tab: key }); } catch { /* no signals on this page */ }
      if (props.url) {
        const url = new URL(location.href);
        if (key === tabs()[0]?.dataset.tab) url.searchParams.delete("tab");
        else url.searchParams.set("tab", key);
        if (url.href !== location.href) history.replaceState(history.state, "", url);
      }
      host.dispatchEvent(new CustomEvent("sluice-tab", { bubbles: true, detail: { key } }));
    };
    on(host, "click", (event) => {
      const tab = event.target.closest?.("[role=tab]");
      if (tab && tab.closest("sluice-tabs") === host) choose(tab.dataset.tab);
    });
    // arrows move along the bar (wrapping), Home and End to its ends; each move chooses the tab
    on(host, "keydown", (event) => {
      const tab = event.target.closest?.("[role=tab]");
      if (!tab || tab.closest("sluice-tabs") !== host || event.altKey || event.ctrlKey || event.metaKey) return;
      const all = tabs(), at = all.indexOf(tab);
      const next = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: all.length - 1 }[event.key];
      if (next === undefined) return;
      event.preventDefault();
      choose(all[(next + all.length) % all.length].dataset.tab, { focus: true });
    });
    on(host, "scroll", (event) => { if (event.target === bar()) overflow(); }, { capture: true });
    on(window, "resize", overflow);
    document.fonts?.ready.then(overflow);

    // deep links: the tab holding the target is chosen, a fold around it opened, and it is
    // brought into view
    const reveal = (id, { scroll = true } = {}) => {
      let target = null;
      try { target = id && document.getElementById(decodeURIComponent(id)); } catch { return false; }
      const panel = target?.closest(".tp");
      if (!panel || panel.closest("sluice-tabs") !== host) return false;
      if ($$.tab !== panel.dataset.tab) choose(panel.dataset.tab);
      for (let d = target.closest("details:not([open])"); d; d = d.parentElement?.closest("details:not([open])")) d.open = true;
      if (target.matches("details:not([open])")) target.open = true;
      if (scroll) target.scrollIntoView({ block: "start" });
      // a box the link leads to takes the focus (the band's Retry with feedback)
      if (target.matches("textarea, input:not([type=hidden])")) target.focus({ preventScroll: true });
      return true;
    };
    on(document, "click", (event) => {
      const a = event.target.closest?.("a[href^='#'], a[data-tab-to]");
      if (!a || event.defaultPrevented || !plainClick(event)) return;
      if (a.dataset.tabTo && !a.getAttribute("href")?.startsWith("#")) {
        if (a.closest("sluice-tabs") !== host) return;
        event.preventDefault();
        choose(a.dataset.tabTo);
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
    on(window, "hashchange", fromHash);
    const drawn = new MutationObserver(() => apply($$.tab));
    drawn.observe(host, { childList: true, subtree: true });
    cleanup(() => drawn.disconnect());
    apply($$.tab);
    fromHash();
  },
});

// ---- sluice-fold -------------------------------------------------------------------------------
// Two shapes. A long text: its `.clip` (its first lines, faded) and the `details.fold-toggle`
// under it ("Show all", "Show less"); the toggle and the fade are said only when the text is
// really cut (`fits` otherwise). Or a `details.more-fold` (a project's description, a board's
// document), kept open per `remember` key in this browser, or opened on a wide screen (`wide`)
// until the reader folds or opens it by hand. Folding either brings its head back into view;
// a `.fold-less` at a More's end folds it from where the reader is.
const WIDE = matchMedia("(min-width: 1280px)");
define("sluice-fold", {
  props: ({ string, bool }) => ({ remember: string, wide: bool }),
  manifest: {
    slots: [{ name: "clip", description: "A long text's .clip (its first lines, faded)." },
            { name: "toggle", description: "Then its details.fold-toggle: Show all, Show less." },
            { name: "details", description: "Or a details.more-fold with its summary's two words and what it holds." }],
    events: [],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    const more = () => host.querySelector(":scope > details.more-fold");
    const toggle = () => host.querySelector(":scope > details.fold-toggle");
    const clip = () => host.querySelector(":scope > .clip");
    // a long text whose whole fits its first lines needs no Show all
    const measure = () => {
      const c = clip(), t = toggle();
      // not drawn (a panel not chosen, a fold shut): measured when it shows
      if (!c || !t || t.open || !c.clientHeight) return;
      const fits = c.scrollHeight <= c.clientHeight + 1;
      if (host.hasAttribute("fits") !== fits) host.toggleAttribute("fits", fits);
    };
    if (clip()) {
      const sizes = new ResizeObserver(measure);
      sizes.observe(host);
      const changes = new MutationObserver(measure);
      changes.observe(host, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["fits"] });
      cleanup(() => { sizes.disconnect(); changes.disconnect(); });
      document.fonts?.ready.then(measure);
    }
    let setting = false;  // opened here, not by the reader
    const set = (open) => {
      const d = more();
      if (!d || d.open === open) return;
      setting = true;
      d.open = open;
    };
    let touched = false;
    if (props.remember && store.get(props.remember) === "open") set(true);
    else if (props.wide) set(WIDE.matches);
    const wide = () => { if (props.wide && !touched && !(props.remember && store.get(props.remember))) set(WIDE.matches); };
    on(WIDE, "change", wide);
    const back = () => {
      const head = host.querySelector(":scope > details > summary") ?? host;
      if (head.getBoundingClientRect().top < 0) head.scrollIntoView({ block: "nearest" });
    };
    on(host, "toggle", (event) => {
      const d = event.target;
      if (d.parentElement !== host) return;
      if (d.matches(".more-fold")) {
        if (setting) { setting = false; return; }
        touched = true;
        if (props.remember) store.set(props.remember, d.open ? "open" : null);
      }
      if (!d.open) back();
      if (d.matches(".fold-toggle") && !d.open) requestAnimationFrame(measure);
    }, { capture: true });
    on(host, "click", (event) => {
      const less = event.target.closest?.(".fold-less");
      const d = more();
      if (!less || !d) return;
      d.open = false;
      const head = d.querySelector(":scope > summary");
      if (head.getBoundingClientRect().top < 0) head.scrollIntoView({ block: "center" });
      head.focus({ preventScroll: true });
    });
    measure();
  },
});

// ---- sluice-confirm ----------------------------------------------------------------------------
// A confirmation (Cancel, Close all, Delete project) stays the `<details class="confirm-flow">`
// the server draws, so a patch morphs it in place: its summary keeps the focus and a click that
// straddles the patch. Its summary opens the page's one `<dialog id="confirmation">` instead,
// titled `heading` (with `ref-id` after it in data mono), the form moved into it while it is
// open; focus starts on the keep button, stays in the dialog and goes back to the opener. While
// `disabled` it does nothing. Without script the details open the same form inline.
let confirming = null;
define("sluice-confirm", {
  props: ({ string, bool }) => ({ heading: string, refId: string, disabled: bool }),
  manifest: {
    slots: [{ name: "summary", description: "The opener: details.confirm-flow > summary." },
            { name: "form", description: "What is confirmed: its .confirm-copy, its fields, a primary or danger button and [data-keep]." }],
    events: [{ name: "sluice-confirm-open", description: "Its dialog opened." },
             { name: "sluice-confirm-close", description: "Its dialog closed; its form is back in its details." }],
  },
  setup({ host, props, observeProps, cleanup }) {
    const on = listening(cleanup);
    const dialog = document.querySelector("#confirmation");
    const summary = () => host.querySelector(":scope > details.confirm-flow > summary");
    const mark = () => {
      const s = summary();
      const value = String(Boolean(props.disabled));
      if (s && s.getAttribute("aria-disabled") !== value) s.setAttribute("aria-disabled", value);
    };
    observeProps(mark, "disabled");
    mark();
    on(host, "click", (event) => {
      const s = event.target.closest?.("details.confirm-flow > summary");
      if (s) {
        if (!dialog) return;
        event.preventDefault();
        if (props.disabled || dialog.open) return;
        open(s);
        return;
      }
      // keep, inline (no dialog): fold the form away
      if (event.target.closest?.("[data-keep]")) host.querySelector(":scope > details.confirm-flow").open = false;
    });
    const open = (opener) => {
      const flow = opener.parentElement;
      const form = flow.querySelector(":scope > form");
      if (!form) return;
      const place = document.createComment("confirmation form");
      form.replaceWith(place);
      confirming = { host, form, place, flow, opener, heading: props.heading };
      const head = dialog.querySelector("#confirmation-title");
      head.textContent = props.heading;
      if (props.refId) {
        // the question mark ends the title; the id it names goes under it
        const id = Object.assign(document.createElement("code"), { className: "sref-id", textContent: props.refId });
        head.append("?", " ", id);
      }
      dialog.querySelector("#confirmation-body").replaceChildren(form);
      form.querySelector(".confirm-copy").id = "confirmation-copy";
      dialog.setAttribute("aria-describedby", "confirmation-copy");
      dialog.showModal();
      form.querySelector("[data-keep]").focus();
      host.dispatchEvent(new CustomEvent("sluice-confirm-open", { bubbles: true }));
    };
  },
});
// the one dialog's own behaviour: keep closes it, Tab stays in it, closing puts the form back
{
  const dialog = document.querySelector("#confirmation");
  dialog?.addEventListener("close", () => {
    if (!confirming) return;
    const { host, form, place, flow, opener, heading } = confirming;
    confirming = null;
    form.querySelector(".confirm-copy")?.removeAttribute("id");
    // a patch while it was open may have drawn the details a new form: keep that one
    if (place.isConnected && !flow.querySelector(":scope > form")) place.replaceWith(form);
    else { place.remove(); form.remove(); }
    const back = opener.isConnected ? opener
      : [...document.querySelectorAll("sluice-confirm")].find((c) => c.heading === heading)?.querySelector("details.confirm-flow > summary");
    back?.focus();
    host.dispatchEvent(new CustomEvent("sluice-confirm-close", { bubbles: true }));
  });
  dialog?.addEventListener("click", (event) => {
    if (event.target.closest?.("[data-keep]")) dialog.close();
  });
  dialog?.addEventListener("keydown", (event) => {
    if (event.key !== "Tab") return;
    const controls = [...dialog.querySelectorAll("button:not(:disabled), textarea, input:not([type=hidden])")];
    const first = controls[0], last = controls.at(-1);
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  });
}

// ---- sluice-conversation -----------------------------------------------------------------------
// One conversation and what its page draws before it (a note's head, a thread's line). "Jump to
// latest" brings its newest message into view and gives it the focus; a thread's own page
// (`start` "end") opens at its end when its address names no message. Mark read is the only way
// a note is read: it sends the watermark of what the page drew of its thread, never more, and
// the page's stream then moves it under "Read today"; "Mark all read" marks every conversation
// in its section. A mark that did not go through is said once, at the top of the notes, with
// Try again.
const unsent = new Set();
function readFailed(button) {
  unsent.add(button);
  const group = button.closest("section.notes, #messages-view, main") ?? document.body;
  let status = group.querySelector(":scope .read-status");
  if (!status) {
    status = Object.assign(document.createElement("p"), { className: "meta read-status", role: "status" });
    const retry = Object.assign(document.createElement("button"), { type: "button", className: "link-button", textContent: "Try again" });
    retry.addEventListener("click", () => {
      const again = [...unsent];
      unsent.clear();
      status.remove();
      again.forEach(markRead);
    });
    status.append(document.createElement("span"), " ", retry);
    const help = group.querySelector(".notes-help");
    if (help) help.after(status); else group.prepend(status);
  }
  status.firstChild.textContent = `${unsent.size === 1 ? "A note was" : `${unsent.size} notes were`} not marked read: sluice did not take it.`;
}
async function markRead(button) {
  if (!button.isConnected || button.disabled) return;
  button.disabled = true;
  try {
    const response = await fetch(button.dataset.readUrl, { method: "POST", headers: { "content-type": "application/json" },
                                                           body: JSON.stringify({ thread: button.dataset.thread, through: Number(button.dataset.through) }) });
    if (!response.ok) throw new Error("Could not mark this note read.");
    button.textContent = "Marked read";
    button.dispatchEvent(new CustomEvent("sluice-read", { bubbles: true, detail: { thread: button.dataset.thread } }));
  } catch {
    button.disabled = false;
    readFailed(button);
  }
}
define("sluice-conversation", {
  props: ({ string }) => ({ start: string }),
  manifest: {
    slots: [{ name: "lead", description: "What its page draws before it: a note's head or a thread's line, with its Mark read." },
            { name: "list", description: "The .convo: its head, its groups of messages, its message box." }],
    events: [{ name: "sluice-read", description: "A Mark read went through; detail.thread is its thread." }],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    on(host, "click", (event) => {
      const mark = event.target.closest?.("button.mark-read");
      if (mark) { markRead(mark); return; }
      const jump = event.target.closest?.("a.jump");
      if (!jump || !plainClick(event)) return;
      const last = [...host.querySelectorAll(".convo-list [id^='message-']")].at(-1);
      if (!last) return;
      event.preventDefault();
      last.scrollIntoView({ block: "center", behavior: smooth() });
      if (!last.hasAttribute("tabindex")) last.setAttribute("tabindex", "-1");
      last.focus({ preventScroll: true });
    });
    on(document, "click", (event) => {
      const all = event.target.closest?.("[data-mark-all]");
      const section = all?.closest("section");
      if (section?.contains(host)) host.querySelectorAll("button.mark-read").forEach(markRead);
    });
    if (props.start === "end" && !location.hash) {
      const end = host.querySelector(".composer") ?? [...host.querySelectorAll(".convo-list > li")].at(-1);
      if (end) requestAnimationFrame(() => end.scrollIntoView({ block: "end" }));
    }
  },
});

// ---- sluice-composer ---------------------------------------------------------------------------
// The message box: a note or a question to its step or the orchestrator, posted as JSON without
// leaving the page (Ctrl or Cmd with Enter sends). The conversation's stream brings the message
// in; the box keeps what is typed until then (`data-ignore-morph`). Without script it posts as a
// form.
define("sluice-composer", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "form", description: "form.composer: its recipient, its text, Ask a question, Send and its .ou-status." }],
    events: [{ name: "sluice-sent", description: "The message was taken; detail.to is its recipient." }],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    on(host, "keydown", (event) => {
      if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && event.target.matches?.("textarea")) {
        event.preventDefault();
        event.target.form?.requestSubmit();
      }
    });
    on(host, "submit", async (event) => {
      const form = event.target;
      if (!form.matches?.("form.composer")) return;
      event.preventDefault();
      const status = form.querySelector(".ou-status");
      const button = form.querySelector("button");
      const data = new FormData(form);
      button.disabled = true;
      try {
        const response = await fetch(form.getAttribute("action"), { method: "POST", headers: { "content-type": "application/json" },
                                                     body: JSON.stringify({ body: data.get("body"), to: data.get("to"), ask: data.get("ask") === "true" }) });
        const result = await response.json().catch(() => ({}));
        if (!response.ok) throw new Error(result.message ?? "Sluice did not take the message.");
        status.textContent = "Sent.";
        form.reset();
        host.dispatchEvent(new CustomEvent("sluice-sent", { bubbles: true, detail: { to: data.get("to") } }));
      } catch (error) {
        status.textContent = unreached(error);
      } finally {
        button.disabled = false;
      }
    });
  },
});

// ---- sluice-answer -----------------------------------------------------------------------------
// A question's answer: Answer opens its box under the buttons, which keep their places (the
// box holds the question's own form, which openui.js draws from its OpenUI program, loaded once
// a page has one), and Close question closes it at once, without leaving the page, its line
// then saying so and taking the focus as an answer's does. Without script the box stands open
// with its words-only form.
let openui = null;
define("sluice-answer", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "actions", description: ".q-actions: button.q-toggle (aria-controls its box) and form.q-close." },
            { name: "box", description: ".q-box with its .answer[data-url] area and its words-only form." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    if (!openui && host.querySelector(".answer[data-url]")) openui = import("/static/openui.js");
    on(host, "click", (event) => {
      const toggle = event.target.closest?.("button.q-toggle");
      if (!toggle) return;
      const box = document.getElementById(toggle.getAttribute("aria-controls"));
      if (!box) return;
      const open = !box.hasAttribute("data-open");
      box.toggleAttribute("data-open", open);
      toggle.setAttribute("aria-expanded", String(open));
      if (open) box.querySelector("textarea, input, select, button")?.focus({ preventScroll: true });
    });
    on(host, "submit", async (event) => {
      const form = event.target;
      if (!form.matches?.("form.q-close")) return;
      event.preventDefault();
      const button = form.querySelector("button");
      button.disabled = true;
      try {
        // the attribute: its hidden input named "action" shadows `form.action`
        const response = await fetch(form.getAttribute("action"), { method: "POST", headers: { "content-type": "application/json" },
                                                    body: JSON.stringify({ body: "", answer: { action: "close" } }) });
        if (!response.ok) throw new Error((await response.json().catch(() => ({}))).message ?? "Sluice did not close it.");
        // a question with an answer box becomes the server's "Closed just now: …" line, which
        // takes the focus (openui.js); a stopped one's row says Closed, focused, in its place
        const area = host.querySelector(".answer[data-url]");
        const ui = area && openui ? await openui : null;
        if (ui?.answered) return ui.answered(area, true);
        form.closest("li, article.item")?.classList.add("closing");
        const said = Object.assign(document.createElement("span"), { className: "meta", tabIndex: -1, textContent: "Closed." });
        said.setAttribute("role", "status");
        form.replaceChildren(said);
        said.focus();
      } catch (error) {
        button.disabled = false;
        form.querySelector(".ou-status")?.remove();
        form.append(Object.assign(document.createElement("span"), { className: "ou-status", role: "status", textContent: ` ${unreached(error)}` }));
      }
    });
  },
});

// ---- sluice-menu -------------------------------------------------------------------------------
// A menu behind its summary (the project switcher, display preferences, an item's Details
// behind its "⋯"): a `<details>` that opens and works without script. Its summary says
// aria-expanded; a click elsewhere or Escape closes it, the focus back on its summary;
// ArrowDown from the summary goes into it, and the arrows, Home and End move through its links
// and buttons (a radio keeps its own arrows).
define("sluice-menu", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "summary", description: "Its details' summary, the opener (its accessible name says what it holds)." },
            { name: "menu", description: "Its .menu: links, buttons or a form." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    const details = () => host.querySelector(":scope > details");
    const said = () => {
      const d = details(), summary = d?.querySelector(":scope > summary");
      if (summary && summary.getAttribute("aria-expanded") !== String(d.open)) summary.setAttribute("aria-expanded", String(d.open));
    };
    // an open panel stays inside the window: one anchored by its right edge near the window's
    // left (a short name's "⋯", a module at the sheet's left) moves right until it fits, and the
    // other way round; a phone's sheet is fixed to the window already
    const fit = () => {
      const d = details(), panel = d?.querySelector(":scope > .dm-p");
      if (!panel) return;
      panel.style.translate = "";
      if (!d.open || getComputedStyle(panel).position === "fixed") return;
      const r = panel.getBoundingClientRect(), edge = 8, width = document.documentElement.clientWidth;
      const dx = r.left < edge ? edge - r.left : r.right > width - edge ? width - edge - r.right : 0;
      if (dx) panel.style.translate = `${Math.round(dx)}px 0`;
    };
    on(host, "toggle", () => { said(); fit(); }, { capture: true });
    on(window, "resize", fit);
    said();
    fit();
    const items = () => [...(details()?.querySelectorAll(".menu a[href], .menu button:not([hidden]), .menu input:not([type=hidden])") ?? [])]
      .filter((el) => el.checkVisibility?.() ?? true);
    on(document, "click", (event) => {
      const d = details();
      if (d?.open && !d.contains(event.target)) d.open = false;
    });
    on(host, "keydown", (event) => {
      const d = details();
      if (!d) return;
      if (event.key === "Escape" && d.open) {
        event.preventDefault();
        event.stopPropagation();
        d.open = false;
        d.querySelector(":scope > summary").focus();
        return;
      }
      if (event.target.matches?.("input[type=radio], input[type=search], textarea")) return;
      const list = items();
      const at = list.indexOf(event.target);
      const summary = event.target === d.querySelector(":scope > summary");
      let next;
      if (summary && event.key === "ArrowDown") {
        d.open = true;
        next = 0;
      } else if (at >= 0) {
        next = { ArrowDown: (at + 1) % list.length, ArrowUp: (at - 1 + list.length) % list.length,
                 Home: 0, End: list.length - 1 }[event.key];
      }
      if (next === undefined) return;
      event.preventDefault();
      requestAnimationFrame(() => items()[next]?.focus());
    });
  },
});

// ---- sluice-copy -------------------------------------------------------------------------------
// An id or a SHA with a button that copies `value` (the button is there only with script); it
// says Copied for a moment, to the eye and to a screen reader.
// The page's keys, listed in display preferences: / finds on the page (on a step's or unit's
// page, on its plan: `a[data-find-at]`), g then a letter goes to a section (its link in the
// list), ? opens the list. A key typed into a field, with a
// modifier, or under an open dialog is the field's or the dialog's, never one of these.
define("sluice-keys", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "list", description: ".keys: a dl of each key and what it does; each g key's link (a[data-go]) is where it goes." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    let go = 0;
    cleanup(() => clearTimeout(go));
    const typing = (target) =>
      target instanceof Element && Boolean(target.closest("input, textarea, select, [contenteditable]:not([contenteditable=false])"));
    const finder = () => [...document.querySelectorAll(":is(main, .subnav) [data-find]")].find((el) => el.checkVisibility?.() ?? true);
    // arrived from `/` on a page with nothing to find: the find takes the focus, once
    if (location.hash === "#find") {
      history.replaceState(history.state, "", location.pathname + location.search);
      requestAnimationFrame(() => finder()?.focus());
    }
    on(document, "keydown", (event) => {
      if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return;
      if (typing(event.target) || document.querySelector("dialog[open]")) return;
      if (go) {
        clearTimeout(go);
        go = 0;
        const link = host.querySelector(`a[data-go="${CSS.escape(event.key.toLowerCase())}"]`);
        if (!link) return;
        event.preventDefault();
        location.assign(link.href);
        return;
      }
      if (event.key === "g") {
        go = setTimeout(() => { go = 0; }, 1500);
        return;
      }
      if (event.key === "/") {
        const find = finder();
        // a page with nothing to find (a step's or a unit's) finds on its plan
        const away = find ? null : document.querySelector("a[data-find-at]");
        if (!find && !away) return;
        event.preventDefault();
        if (away) {
          location.assign(away.dataset.findAt);
          return;
        }
        find.focus();
        find.select?.();
        return;
      }
      if (event.key === "?") {
        const menu = host.closest("details");
        if (!menu) return;
        event.preventDefault();
        menu.open = true;
        requestAnimationFrame(() => host.querySelector("a[data-go]")?.focus());
      }
    });
  },
});
define("sluice-copy", {
  props: ({ string }) => ({ value: string }),
  manifest: {
    slots: [{ name: "text", description: "The value in data mono." },
            { name: "button", description: "button.copy with the copy and check icons." }],
    events: [{ name: "sluice-copied", description: "The value is on the clipboard." }],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    let timer = 0;
    cleanup(() => clearTimeout(timer));
    const write = async (text) => {
      try { await navigator.clipboard.writeText(text); return true; } catch { /* not a secure page: below */ }
      const scratch = Object.assign(document.createElement("textarea"), { value: text });
      scratch.style.cssText = "position:fixed;opacity:0;pointer-events:none";
      document.body.append(scratch);
      scratch.select();
      const done = document.execCommand("copy");
      scratch.remove();
      return done;
    };
    on(host, "click", async (event) => {
      const button = event.target.closest?.("button.copy");
      if (!button) return;
      if (!(await write(props.value))) return;
      let said = host.querySelector(":scope > .copy-said");
      if (!said) {
        said = Object.assign(document.createElement("span"), { className: "copy-said vh", role: "status" });
        host.append(said);
      }
      said.textContent = "Copied";
      button.setAttribute("data-copied", "");
      clearTimeout(timer);
      timer = setTimeout(() => { button.removeAttribute("data-copied"); said.textContent = ""; }, 1600);
      host.dispatchEvent(new CustomEvent("sluice-copied", { bubbles: true, detail: { value: props.value } }));
    });
  },
});

// ---- sluice-toggle -----------------------------------------------------------------------------
// A display setting applied at once and kept by posting it to /settings, which sets the cookie
// every page is drawn from: `setting` "types" (a Types switch's aria-pressed, or the display
// preferences' checkbox; every one on the page follows, and `show-types` on <html>) or "theme"
// (its id as `data-theme` on <html>: a complete theme, its palette and its scheme).
const keepSetting = (name, value) =>
  fetch("/settings", { method: "POST", keepalive: true, body: new URLSearchParams({ [name]: value }) }).catch(() => {});
define("sluice-toggle", {
  props: ({ string }) => ({ setting: string }),
  manifest: {
    slots: [{ name: "control", description: "button.types-toggle[aria-pressed], a types checkbox, or the theme radios." }],
    events: [{ name: "sluice-setting", description: "A setting changed (on document); detail.setting and detail.value." }],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    const sync = () => {
      if (props.setting !== "types") return;
      const shown = root.classList.contains("show-types");
      for (const b of host.querySelectorAll(".types-toggle")) {
        if (b.getAttribute("aria-pressed") !== String(shown)) b.setAttribute("aria-pressed", String(shown));
      }
      for (const box of host.querySelectorAll("input[type=checkbox][name=types]")) box.checked = shown;
    };
    const say = (value) => document.dispatchEvent(new CustomEvent("sluice-setting", { detail: { setting: props.setting, value } }));
    const types = (shown) => {
      root.classList.toggle("show-types", shown);
      keepSetting("types", shown ? "1" : "0");
      say(shown);
    };
    on(host, "click", (event) => {
      if (event.target.closest?.(".types-toggle")) types(!root.classList.contains("show-types"));
    });
    on(host, "change", (event) => {
      const input = event.target;
      if (input.name === "types" && input.type === "checkbox") types(input.checked);
      if (input.name === "theme" && input.value) {
        root.dataset.theme = input.value;
        keepSetting("theme", input.value);
        say(input.value);
      }
    });
    on(document, "sluice-setting", sync);
    sync();
  },
});

// ---- sluice-search -----------------------------------------------------------------------------
// `mode` "stream": the board's search and filters (its form) apply as they change, the search as
// one types: the address follows (replaceState) and the page's stream restarts on the new
// query under `base`, so its patches draw the board as filtered. Escape clears the search
// first, and only that (not also the open step). `mode` "filter": its field hides the items it
// holds (`[data-find]`, their words) that do not hold every word typed, a group
// (`[data-find-group]`) with none left, and says how many match in its `[data-find-status]`.
// `mode` "submit": a plain form of filters. In "filter" and "submit" a `form[data-applies]`
// sends itself when any field in it changes (a select, a checkbox, or a text field left with
// new words), so its Apply (`.apply`, kept as the form's default button for Enter in a text
// field) hides while the component runs.
define("sluice-search", {
  props: ({ string, oneOf }) => ({ mode: oneOf("stream", "filter", "submit"), base: string }),
  manifest: {
    slots: [{ name: "form", description: "stream: form.board-tools with input[name=q], its selects and [data-clear-q]." },
            { name: "list", description: "filter: its input[type=search], the [data-find] items, [data-find-group]s and [data-find-status]." }],
    events: [{ name: "sluice-stream-restart", description: "On window: the page's stream is ended on purpose, to open on a new query." }],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    if (props.mode !== "stream") {
      on(host, "change", (event) => {
        const form = event.target.closest?.("form[data-applies]");
        // a text field's change fires when it is left with new words (or on Enter): it applies
        // then too, so a filter typed and tabbed away from never silently waits
        if (form && event.target.matches("select, input")) form.requestSubmit();
      });
    }
    if (props.mode === "submit") return;
    if (props.mode === "filter") return filter(host, on, cleanup);
    const form = () => host.querySelector("form");
    let typing = 0;
    cleanup(() => clearTimeout(typing));
    const restart = (url) => {
      const main = document.querySelector("main[data-init]");
      const init = main?.getAttribute("data-init");
      if (!init) return;
      const next = init.replace(/@get\('[^']*'/, `@get('${url}'`);
      if (next === init) return;
      window.dispatchEvent(new Event("sluice-stream-restart"));
      main.setAttribute("data-init", next);
    };
    const apply = () => {
      clearTimeout(typing);
      const f = form();
      if (!f) return;
      const params = new URLSearchParams();
      const here = new URLSearchParams(location.search);
      for (const [name, value] of new FormData(f)) {
        const v = String(value).trim();
        if (name === "q" ? v : true) params.set(name, v);
      }
      if (here.get("tag")) params.set("tag", here.get("tag"));
      // the address leaves out the defaults and keeps the open step's tab; the stream asks for
      // exactly what the page shows
      const shown = new URLSearchParams(params);
      if (shown.get("order") === "live") shown.delete("order");
      if (shown.get("show") === "all") shown.delete("show");
      if (here.get("tab")) shown.set("tab", here.get("tab"));
      const address = `${location.pathname}${shown.size ? `?${shown}` : ""}${location.hash}`;
      if (address !== `${location.pathname}${location.search}${location.hash}`) history.replaceState(history.state, "", address);
      restart(`${props.base}/stream?${params}`);
    };
    on(host, "input", (event) => {
      if (!event.target.matches?.("input[name=q]")) return;
      clearTimeout(typing);
      typing = setTimeout(apply, 200);
    });
    on(host, "change", (event) => { if (event.target.matches?.("select")) apply(); });
    on(host, "submit", (event) => { event.preventDefault(); apply(); });
    const clear = (input) => { input.value = ""; apply(); };
    on(host, "click", (event) => {
      const button = event.target.closest?.("[data-clear-q]");
      const input = form()?.querySelector("input[name=q]");
      if (!button || !input || !plainClick(event)) return;
      event.preventDefault();
      input.focus();
      clear(input);
    });
    on(host, "keydown", (event) => {
      const input = event.target;
      if (event.key !== "Escape" || !input.matches?.("input[name=q]") || !input.value) return;
      event.preventDefault();
      event.stopPropagation();
      clear(input);
    });
  },
});
function filter(host, on, cleanup) {
  const input = host.querySelector("input[type=search]");
  if (!input) return;
  const apply = () => {
    const words = input.value.toLowerCase().split(/\s+/).filter(Boolean);
    let shown = 0;
    for (const item of host.querySelectorAll("[data-find]")) {
      const hit = words.every((w) => item.dataset.find.includes(w));
      if (item.hidden === hit) item.hidden = !hit;
      if (hit) shown++;
    }
    for (const group of host.querySelectorAll("[data-find-group]")) {
      const hide = words.length > 0 && !group.querySelector("[data-find]:not([hidden])");
      if (group.hidden !== hide) group.hidden = hide;
    }
    const status = host.querySelector("[data-find-status]");
    if (!status) return;
    if (status.hidden !== !words.length) status.hidden = !words.length;
    const said = !words.length ? "" : shown === 0 ? `${status.dataset.none} “${input.value}”.`
      : `${shown} ${shown === 1 ? status.dataset.one : status.dataset.many}.`;
    if (status.textContent !== said) status.textContent = said;
  };
  on(input, "input", apply);
  on(input, "keydown", (event) => {
    if (event.key === "Escape" && input.value) { event.preventDefault(); input.value = ""; apply(); }
  });
  // a patch draws the list again, every item shown: the search applies again (it writes only
  // what changed, so its own words settle at once)
  const drawn = new MutationObserver(() => { if (input.value) apply(); });
  drawn.observe(host, { childList: true, subtree: true });
  cleanup(() => drawn.disconnect());
  apply();
}

// ---- sluice-banner -----------------------------------------------------------------------------
// `kind` "stream": how the page's stream stands. Datastar owns its retries and cancellation;
// live, the line is hidden; while Datastar retries (an error, or the stream ended), "Updates
// paused. Reconnecting…"; once it gives up (its retries ran out after about three minutes, or
// the server ended the stream for good), "Updates stopped at 14:02." with Reconnect: it never
// gives up silently, and going online, the tab showing or the window's focus opens it again.
// `kind` "release": the first batch of every connection names the build that drew it (`rel`); a
// page drawn by another says "sluice was updated" with Reload, calmly.
const clock = (at) => at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
define("sluice-banner", {
  props: ({ oneOf }) => ({ kind: oneOf("stream", "release") }),
  manifest: {
    slots: [{ name: "words", description: "stream: .stream-words, which it rewrites; release: its words." },
            { name: "button", description: ".stream-retry (Reconnect) or .release-reload (Reload)." }],
    events: [],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    const signals = (event) => {
      try { return JSON.parse(event.detail.argsRaw?.signals ?? "{}"); } catch { return {}; }
    };
    if (props.kind === "release") {
      on(host, "click", (event) => { if (event.target.closest?.(".release-reload")) location.reload(); });
      on(document, "datastar-fetch", (event) => {
        if (event.detail.type !== "datastar-patch-signals") return;
        const { rel } = signals(event);
        if (rel) host.hidden = rel === document.body.dataset.release;
      });
      return;
    }
    const mainStream = () => document.querySelector("main[data-init]");
    let phase = "live";
    // a page that points its stream at a new query (the board's search), or reconnects it, ends
    // the old request on purpose: its end is not a lost connection
    let restarts = 0;
    on(window, "sluice-stream-restart", () => { if (phase !== "stopped") restarts++; });
    const show = (next) => {
      phase = next;
      host.hidden = next === "live";
      // the band's live line reads this: "Live" only while the stream is
      root.dataset.stream = next;
      const words = host.querySelector(".stream-words");
      if (next === "paused") words.textContent = "Updates paused. Reconnecting…";
      if (next === "stopped") words.textContent = `Updates stopped at ${clock(new Date())}.`;
    };
    /** Open the page's stream again, now: Datastar ends the old request ('cleanup') and starts anew. */
    const reconnect = () => {
      const main = mainStream();
      const init = main?.getAttribute("data-init");
      if (!init) return;
      window.dispatchEvent(new Event("sluice-stream-restart"));
      show("paused");
      main.removeAttribute("data-init");
      setTimeout(() => main.setAttribute("data-init", init));
    };
    on(document, "datastar-fetch", (event) => {
      // the page's own stream (its attribute is briefly gone while it reconnects), never the drawer's
      if (event.detail.el !== document.querySelector("main#content")) return;
      const type = event.detail.type;
      if (type === "finished") {
        if (restarts) { restarts--; return; }
        show("stopped");
      } else if (type === "retries-failed") {
        show("stopped");
      } else if (type === "error" || type === "retrying") {
        if (phase !== "stopped") show("paused");
      } else if (type === "datastar-patch-signals") {
        // what the stream itself says (Datastar's signal events fire only for a value that moved)
        const { stale } = signals(event);
        if (stale === false) show("live");
        if (stale === true && phase === "live") show("paused");
      }
    });
    const revive = () => { if (phase === "stopped" && !document.hidden) reconnect(); };
    on(window, "online", revive);
    on(window, "focus", revive);
    on(document, "visibilitychange", revive);
    on(host, "click", (event) => { if (event.target.closest?.(".stream-retry")) reconnect(); });
  },
});

// ---- sluice-splitter ---------------------------------------------------------------------------
// The handle between the plan and the board beside it (`aria-controls` the board): drag it, or
// the arrow keys (16px, 64px with Shift), Home and End; a double-click resets. The board's width
// is `--board-w` on its `frame` (whose `style` a patch keeps), at least `min` and leaving the
// plan `rest`, kept per project under `store`; the separator's values follow the board as laid
// out. While it is dragged the plan's lines hide (`resizing` on <html>) and are drawn once at
// the end ("sluice-resized").
define("sluice-splitter", {
  props: ({ string, number }) => ({ frame: string, store: string, min: number.default(320), rest: number.default(560) }),
  manifest: {
    slots: [{ name: "grip", description: "Its grip icon; the host itself is the separator." }],
    events: [{ name: "sluice-resized", description: "On window: the board's width changed; the plan redraws its lines." }],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    const frame = () => document.getElementById(props.frame);
    const pane = () => document.getElementById(host.getAttribute("aria-controls"));
    const bounds = () => {
      const total = frame().getBoundingClientRect().width;
      const gap = host.getBoundingClientRect().width;
      const max = Math.floor(Math.min(total * 0.65, total - props.rest - gap));
      return { min: props.min, max: Math.max(props.min, max) };
    };
    const clamp = (w, { min, max }) => Math.round(Math.min(max, Math.max(min, w)));
    const values = () => {
      const board = pane();
      if (!frame() || !board || !host.checkVisibility?.()) return;
      const b = bounds();
      const now = Math.round(board.getBoundingClientRect().width);
      for (const [name, value] of [["aria-valuemin", b.min], ["aria-valuemax", b.max],
                                   ["aria-valuenow", clamp(now, b)], ["aria-valuetext", `Board ${now} pixels wide`]]) {
        if (host.getAttribute(name) !== String(value)) host.setAttribute(name, value);
      }
    };
    const resized = () => window.dispatchEvent(new Event("sluice-resized"));
    const set = (width, keep) => {
      const f = frame();
      if (width == null) f.style.removeProperty("--board-w");
      else f.style.setProperty("--board-w", `${width}px`);
      if (keep) store.set(props.store, width == null ? null : String(width));
    };
    // the page's grid keeps it within bounds when the window is narrower than when it was set
    const saved = Number(store.get(props.store));
    if (frame() && saved >= props.min && !frame().style.getPropertyValue("--board-w")) set(Math.round(saved), false);
    const sizes = new ResizeObserver(() => requestAnimationFrame(values));
    for (const el of [frame(), pane(), host]) if (el) sizes.observe(el);
    cleanup(() => sizes.disconnect());
    let drag = null, frameId = 0;
    const end = () => {
      if (!drag) return;
      cancelAnimationFrame(frameId);
      frameId = 0;
      if (drag.width != null) set(drag.width, true);
      drag = null;
      root.classList.remove("resizing");
      resized();
      values();
    };
    on(host, "pointerdown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      host.focus({ preventScroll: true });
      host.setPointerCapture(event.pointerId);
      drag = { x: event.clientX, from: pane().getBoundingClientRect().width, bounds: bounds(), width: null };
      root.classList.add("resizing");
    });
    on(host, "pointermove", (event) => {
      if (!drag) return;
      // the board is on the right: moving the splitter left widens it
      drag.width = clamp(drag.from - (event.clientX - drag.x), drag.bounds);
      if (frameId) return;
      frameId = requestAnimationFrame(() => {
        frameId = 0;
        if (drag?.width != null) frame().style.setProperty("--board-w", `${drag.width}px`);
      });
    });
    for (const type of ["pointerup", "pointercancel", "lostpointercapture"]) on(host, type, end);
    cleanup(() => { if (drag) root.classList.remove("resizing"); });
    on(host, "dblclick", () => { set(null, true); requestAnimationFrame(values); resized(); });
    on(host, "keydown", (event) => {
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      const b = bounds(), now = pane().getBoundingClientRect().width;
      const step = event.shiftKey ? 64 : 16;
      const width = { ArrowLeft: now + step, ArrowRight: now - step, Home: b.min, End: b.max }[event.key];
      if (width == null) return;
      event.preventDefault();
      set(clamp(width, b), true);
      requestAnimationFrame(values);
      resized();
    });
    requestAnimationFrame(values);
  },
});

// ---- sluice-trace ------------------------------------------------------------------------------
// Select to trace (`ui::trace_open`). Each traceable unit is an element with `data-unit` (its
// name), `data-up` and `data-down` (every unit up and down its chain, as the server worked them
// out) and `data-chain` (the sentence that says it); its `[data-trace-pick]` button selects it.
// Selecting marks every unit `data-trace` selected, up, down or faded, opens the selected one's
// `[data-trace-more]` in place, draws the rail (each `.rail-slot`'s `.rail`, in page order,
// gets `data-rail`: top, bottom and a node) and puts the sentence in the line; selecting it
// again, Clear trace or Escape clears. The arrows move between the units' buttons. The choice
// is the host's `selected`, kept through a patch, and drawn again after one.
define("sluice-trace", {
  props: ({ string }) => ({ selected: string }),
  manifest: {
    slots: [{ name: "line", description: "The server's .trace-line: .trace-words (the sentence, polite) and button.trace-clear." },
            { name: "units", description: "Elements with data-unit, data-up, data-down and data-chain, each a .rail-slot with its .rail and a [data-trace-pick] button; section heads may be .rail-slot too." }],
    events: [{ name: "sluice-trace", description: "A unit was traced or the trace cleared; detail.unit is its name or \"\"." }],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    const words = host.querySelector(".trace-words");
    const idle = words?.textContent ?? "";
    const units = () => [...host.querySelectorAll("[data-unit]")];
    const picks = () => [...host.querySelectorAll("[data-trace-pick]")].filter((b) => b.checkVisibility?.() ?? true);
    const draw = () => {
      const id = host.getAttribute("selected") ?? "";
      const unit = id ? units().find((u) => u.dataset.unit === id) : null;
      const set = (attr) => new Set((unit?.dataset[attr] ?? "").split(" ").filter(Boolean));
      const up = set("up"), down = set("down");
      host.toggleAttribute("tracing", !!unit);
      for (const u of units()) {
        const name = u.dataset.unit;
        const role = !unit ? "" : name === id ? "selected" : up.has(name) ? "up" : down.has(name) ? "down" : "faded";
        if (role) { if (u.dataset.trace !== role) u.dataset.trace = role; } else if (u.dataset.trace) delete u.dataset.trace;
        for (const pick of u.querySelectorAll(":scope [data-trace-pick]")) {
          if (pick.closest("[data-unit]") !== u) continue;
          const pressed = String(role === "selected");
          if (pick.getAttribute("aria-pressed") !== pressed) pick.setAttribute("aria-pressed", pressed);
          if (pick.getAttribute("aria-expanded") !== pressed) pick.setAttribute("aria-expanded", pressed);
        }
        for (const more of u.querySelectorAll(":scope [data-trace-more]")) {
          if (more.closest("[data-unit]") === u && more.hidden !== (role !== "selected")) more.hidden = role !== "selected";
        }
      }
      // the rail: from the first lit slot down to the last, through whatever lies between
      const slots = [...host.querySelectorAll(".rail-slot")];
      const lit = slots.map((s) => !!unit && ["selected", "up", "down"].includes(s.dataset.trace));
      const first = lit.indexOf(true), last = lit.lastIndexOf(true);
      const drawn = first >= 0 && last > first;
      slots.forEach((slot, i) => {
        const rail = slot.querySelector(":scope > .rail");
        if (!rail) return;
        const parts = [];
        if (drawn && i > first && i <= last) parts.push("top");
        if (drawn && i >= first && i < last) parts.push("bottom");
        if (drawn && lit[i]) parts.push(slot.dataset.trace === "selected" ? "chosen" : "node");
        const value = parts.join(" ");
        if ((rail.dataset.rail ?? "") !== value) { if (value) rail.dataset.rail = value; else delete rail.dataset.rail; }
      });
      const said = unit ? unit.dataset.chain : idle;
      if (words && words.textContent !== said) words.textContent = said;
      const clear = host.querySelector(".trace-clear");
      if (clear && clear.hidden === !!unit) clear.hidden = !unit;
    };
    const choose = (id) => {
      if (id) host.setAttribute("selected", id); else host.removeAttribute("selected");
      draw();
      host.dispatchEvent(new CustomEvent("sluice-trace", { bubbles: true, detail: { unit: id } }));
    };
    on(host, "click", (event) => {
      if (event.target.closest?.(".trace-clear")) {
        const was = host.getAttribute("selected");
        choose("");
        units().find((u) => u.dataset.unit === was)?.querySelector("[data-trace-pick]")?.focus();
        return;
      }
      const pick = event.target.closest?.("[data-trace-pick]");
      const unit = pick?.closest("[data-unit]");
      if (!unit || !host.contains(unit)) return;
      choose(host.getAttribute("selected") === unit.dataset.unit ? "" : unit.dataset.unit);
    });
    on(host, "keydown", (event) => {
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      if (event.key === "Escape" && host.hasAttribute("selected")) {
        const was = host.getAttribute("selected");
        event.preventDefault();
        choose("");
        units().find((u) => u.dataset.unit === was)?.querySelector("[data-trace-pick]")?.focus();
        return;
      }
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const pick = event.target.closest?.("[data-trace-pick]");
      if (!pick) return;
      const all = picks(), at = all.indexOf(pick);
      const next = all[at + (event.key === "ArrowDown" ? 1 : -1)];
      if (!next) return;
      event.preventDefault();
      next.focus();
    });
    // a patch redraws the units as the server has them: mark them again
    let queued = false;
    const observer = new MutationObserver(() => {
      if (queued) return;
      queued = true;
      queueMicrotask(() => { queued = false; draw(); });
    });
    observer.observe(host, { childList: true, subtree: true });
    cleanup(() => observer.disconnect());
    draw();
  },
});
