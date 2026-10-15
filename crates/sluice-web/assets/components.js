// The dashboard's components (DESIGN.md, Components). Rust draws each one's host and all that is
// in it (`views::ui::rocket`); each is a Rocket component in light DOM whose `setup` only
// enhances what the server drew: listeners, observers, keyboard handling and state kept in the
// host's own attributes, which a stream patch leaves as they are (`data-preserve-attr`). None
// draws content of its own: a patch morphs server HTML into server HTML, and a screen reader
// hears what the server wrote. What Datastar does itself (writes, signals, folds and toggles,
// the streams) is Datastar's, in the markup; what is here is what it does not (DESIGN.md,
// Custom script).
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
const store = {
  get(key) { try { return localStorage.getItem(key); } catch { return null; } },
  set(key, value) {
    try { if (value == null) localStorage.removeItem(key); else localStorage.setItem(key, value); } catch { /* not kept */ }
  },
};

// ---- sluice-tabs -------------------------------------------------------------------------------
// An ARIA tablist (`[role=tab][data-tab]` in its `.tabs`) over its `.tp[data-tab]` panels. The
// choice is `$$tab`, reflected into the host's `current` (kept through a patch), the chosen
// tab's aria-selected and tabindex and its panel's `data-chosen` (the stylesheet hides the rest
// while script runs); also the page's `tab` signal, which the step's stream reads when it
// connects. A patch that draws a new tab or panel is brought in line at once. An anchor to something in a
// panel (`#run-3`, a failing call, `#message-12`, `#activity`) opens its tab on it; in the
// drawer, whose hash is the open step's (`#step:<id>`), such a link never changes the address.
define("sluice-tabs", {
  props: ({ string }) => ({ current: string }),
  manifest: {
    slots: [{ name: "tabs", description: "The server's .tabs[role=tablist] of [role=tab][data-tab] buttons." },
            { name: "panels", description: "Its .tp[data-tab] sections, each under its own head." }],
    events: [{ name: "sluice-tab", description: "A tab was chosen; detail.key is its key." }],
  },
  setup({ host, props, $$, effect, observeProps, cleanup }) {
    const on = listening(cleanup);
    const tabs = () => [...host.querySelectorAll(":scope > .tabs > [role=tab]")];
    const panels = () => [...host.querySelectorAll(":scope > .tp")];
    const valid = (key) => (tabs().some((t) => t.dataset.tab === key) ? key : tabs()[0]?.dataset.tab ?? "");
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
// its summary is `aria-disabled="true"` (drawn by the server, so a patch keeps it current) it
// does nothing.
let confirming = null;
// The confirmation done (kept, taken, or its dialog closed any other way): the form back in its
// details, under its own id, and the focus on its opener. Called at once wherever the dialog is
// closed, not only from its `close` event, which the browser queues as a task: an opener pressed
// in between found its form still in the dialog and opened nothing (a confirmation kept and
// opened again at once). Calling it again does nothing.
function settle() {
  if (!confirming) return;
  const { host, form, id, place, flow, opener, heading } = confirming;
  confirming = null;
  rename(form, "confirming", id);
  form.querySelector(".confirm-copy")?.removeAttribute("id");
  // a patch while it was open may have drawn the details a new form: keep that one
  if (place.isConnected && !flow.querySelector(":scope > form")) place.replaceWith(form);
  else { place.remove(); form.remove(); }
  const back = opener.isConnected ? opener
    : [...document.querySelectorAll("sluice-confirm")].find((c) => c.heading === heading)?.querySelector("details.confirm-flow > summary");
  // an opener in a menu the dialog closed: back onto the menu's own summary
  let to = back;
  while (to && !(to.checkVisibility?.() ?? true)) {
    to = to.closest("details")?.parentElement?.closest("details")?.querySelector(":scope > summary");
  }
  to?.focus();
  host.dispatchEvent(new CustomEvent("sluice-confirm-close", { bubbles: true }));
}
// form `from` (and its notice, `<id>-said`) under id `to`
const rename = (form, from, to) => {
  const said = form.querySelector(`[id="${from}-said"]`);
  if (said) said.id = `${to}-said`;
  form.id = to;
};
define("sluice-confirm", {
  props: ({ string }) => ({ heading: string, refId: string }),
  manifest: {
    slots: [{ name: "summary", description: "The opener: details.confirm-flow > summary." },
            { name: "form", description: "What is confirmed: its .confirm-copy, its fields, a primary or danger button and [data-keep]." }],
    events: [{ name: "sluice-confirm-open", description: "Its dialog opened." },
             { name: "sluice-confirm-close", description: "Its dialog closed; its form is back in its details." }],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    const dialog = document.querySelector("#confirmation");
    on(host, "click", (event) => {
      const s = event.target.closest?.("details.confirm-flow > summary");
      if (!s || !dialog) return;
      event.preventDefault();
      if (s.getAttribute("aria-disabled") === "true" || dialog.open) return;
      open(s);
    });
    const open = (opener) => {
      settle();
      const flow = opener.parentElement;
      const form = flow.querySelector(":scope > form");
      if (!form) return;
      const place = document.createComment("confirmation form");
      form.replaceWith(place);
      // while it is in the dialog the form answers as `confirming`: a patch may draw the details
      // a new form under its own id, and a write's answer finds its notice by id
      const id = form.id;
      rename(form, id, "confirming");
      confirming = { host, form, id, place, flow, opener, heading: props.heading };
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
// ---- sluice-dialog -----------------------------------------------------------------------------
// The page's one confirmation dialog: keep closes it, Tab stays in it, closing puts the form back
// in its details and the focus on its opener. It is a component so that a form moved into it is
// inside a Rocket host, where Rocket finds the action (`@post`) its submit calls.
define("sluice-dialog", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "dialog", description: "dialog#confirmation: its #confirmation-title and #confirmation-body." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    const dialog = host.querySelector(":scope > dialog");
    // Escape, or any other close the page did not make itself. The event is queued: one from a
    // close `done` already settled can arrive after the next confirmation opened, and must
    // leave that one be
    on(dialog, "close", () => { if (!dialog.open) settle(); });
    const done = () => { dialog.close(); settle(); };
    on(dialog, "click", (event) => {
      if (event.target.closest?.("[data-keep]")) done();
    });
    // the write its form sent went through (its answer emptied its notice, never an alert): the
    // confirmation is done (`views::write::done`)
    on(document, "datastar-fetch", (event) => {
      const { type, el } = event.detail;
      if (type !== "finished" || !dialog.open || !(el instanceof HTMLFormElement) || !dialog.contains(el)) return;
      if (el.querySelector(".form-said[role=alert]")?.textContent) return;
      done();
    });
    on(dialog, "keydown", (event) => {
      if (event.key !== "Tab") return;
      const controls = [...dialog.querySelectorAll("button:not(:disabled), textarea, input:not([type=hidden])")];
      const first = controls[0], last = controls.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    });
  },
});

// ---- sluice-conversation -----------------------------------------------------------------------
// One conversation and what its page draws before it (a note's head, a thread's line). "Jump to
// latest" brings its newest message into view and gives it the focus; a thread's own page
// (`start` "end") opens at its end when its address names no message.
define("sluice-conversation", {
  props: ({ string }) => ({ start: string }),
  manifest: {
    slots: [{ name: "lead", description: "What its page draws before it: a note's head or a thread's line, with its Mark read." },
            { name: "list", description: "The .convo: its head, its groups of messages, its message box." }],
    events: [],
  },
  setup({ host, props, cleanup }) {
    const on = listening(cleanup);
    on(host, "click", (event) => {
      const jump = event.target.closest?.("a.jump");
      if (!jump || !plainClick(event)) return;
      const last = [...host.querySelectorAll(".convo-list [id^='message-']")].at(-1);
      if (!last) return;
      event.preventDefault();
      last.scrollIntoView({ block: "center", behavior: smooth() });
      if (!last.hasAttribute("tabindex")) last.setAttribute("tabindex", "-1");
      last.focus({ preventScroll: true });
    });
    if (props.start === "end" && !location.hash) {
      const end = host.querySelector(".composer") ?? [...host.querySelectorAll(".convo-list > li")].at(-1);
      if (end) requestAnimationFrame(() => end.scrollIntoView({ block: "end" }));
    }
  },
});

// ---- sluice-answer -----------------------------------------------------------------------------
// A question's answer: Answer opens its box under the buttons, which keep their places (the
// box holds the question's own form, which openui.js draws from its OpenUI program, loaded once
// a page has one). Its words and Close question are Datastar forms; once one is taken, the
// question gives way at once to the line that says so, which takes the focus (openui.js's
// `answered`), whether or not the page's stream is connected.
let openui = null;
define("sluice-answer", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "actions", description: ".q-actions: button.q-toggle (aria-controls its box) and Close question's sluice-confirm (form.q-close)." },
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
    on(document, "datastar-fetch", async (event) => {
      const form = event.detail.el;
      if (event.detail.type !== "finished" || !(form instanceof HTMLFormElement)) return;
      // Close question is confirmed in the page's dialog, out of this host: its form names
      // the answer area it closes (`ui::close_form`); one with none is redrawn by the stream
      const closed = form.matches("form.q-close");
      const area = closed ? document.getElementById(form.dataset.closes ?? "") : host.querySelector(".answer[data-url]");
      if (!area || !host.contains(area) || (!closed && !(form.matches(".answer form") && host.contains(form)))) return;
      // refused, or never sent: its notice says so, and the question stays
      if (form.querySelector(".form-said[role=alert]")?.textContent) return;
      openui ??= import("/static/openui.js");
      (await openui).answered(area, closed);
    });
  },
});

// ---- sluice-menu -------------------------------------------------------------------------------
// A menu behind its summary (the project switcher, display preferences, an item's Details
// behind its "⋯"): a `<details>`. Its summary says
// aria-expanded; a click elsewhere or Escape closes it, the focus back on its summary;
// ArrowDown from the summary goes into it, and the arrows, Home and End move through its rows,
// links and buttons (a radio keeps its own up and down).
// A menu may hold sub-menus (`details.sub`: display preferences' Theme and Keyboard shortcuts),
// one level shown at a time (style.css): Enter, Space or ArrowRight on a row opens its panel in
// the same menu, the focus on its chosen radio or its first link; its back control,
// ArrowLeft or Backspace returns to the top level, the focus on the row it came from. Only one
// is open at a time, and closing the menu closes them, so it opens again at its top.
define("sluice-menu", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "summary", description: "Its details' summary, the opener (its accessible name says what it holds)." },
            { name: "menu", description: "Its .menu: links, buttons or a form; a details.sub is a sub-menu: its summary the row, its .sub-panel (a .sub-back first) the level it opens." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    const details = () => host.querySelector(":scope > details");
    const subs = () => [...(details()?.querySelectorAll(".menu details.sub") ?? [])];
    const expanded = (d) => {
      const summary = d?.querySelector(":scope > summary");
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
    const shown = (el) => el.checkVisibility?.() ?? true;
    on(host, "toggle", (event) => {
      const d = details(), target = event.target;
      expanded(d);
      fit();
      if (target === d) {
        if (!d.open) for (const sub of subs()) sub.open = false;
        return;
      }
      if (!target.matches?.("details.sub")) return;
      expanded(target);
      if (!d?.open) return;
      const focus = document.activeElement;
      if (target.open) {
        for (const sub of subs()) if (sub !== target) sub.open = false;
        target.closest(".menu")?.scrollTo(0, 0);
        const panel = target.querySelector(":scope > .sub-panel");
        if (panel?.contains(focus)) return;
        const items = [...(panel?.querySelectorAll("a[href], button, input:not([type=hidden])") ?? [])].filter(shown);
        const into = items.find((el) => el.matches("input[type=radio]:checked"))
          ?? items.find((el) => !el.matches(".sub-back")) ?? items[0];
        into?.focus();
      } else if (!focus || focus === document.body || target.contains(focus)) {
        target.querySelector(":scope > summary")?.focus();
      }
    }, { capture: true });
    on(window, "resize", fit);
    expanded(details());
    for (const sub of subs()) expanded(sub);
    fit();
    const items = () => [...(details()?.querySelectorAll(".menu a[href], .menu button:not([hidden]), .menu input:not([type=hidden]), .menu details.sub > summary") ?? [])]
      .filter(shown);
    on(document, "click", (event) => {
      const d = details();
      if (d?.open && !d.contains(event.target)) d.open = false;
    });
    on(host, "click", (event) => {
      const sub = event.target.closest?.(".sub-back")?.closest("details.sub");
      if (sub) sub.open = false;
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
      if (event.ctrlKey || event.metaKey || event.altKey) return;
      // a sub-menu: ArrowRight on its row opens it; inside its panel ArrowLeft or Backspace goes
      // back, and ArrowRight goes nowhere (the levels are across, the items down)
      const sub = event.target.closest?.("details.sub");
      if (sub && d.open && !event.target.matches("input[type=search], textarea")) {
        const row = event.target === sub.querySelector(":scope > summary");
        if (row && event.key === "ArrowRight") {
          event.preventDefault();
          sub.open = true;
          return;
        }
        if (!row && sub.open && (event.key === "ArrowLeft" || event.key === "Backspace")) {
          event.preventDefault();
          sub.open = false;
          return;
        }
        if (!row && event.key === "ArrowRight" && event.target.matches("input[type=radio]")) {
          event.preventDefault();
          return;
        }
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
    const finder = () => [...document.querySelectorAll(":is(main, .site-head) [data-find]")].find((el) => el.checkVisibility?.() ?? true);
    // arrived from `/` on a page with nothing to find: the find takes the focus
    if (location.hash === "#find") requestAnimationFrame(() => finder()?.focus());
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
        // its panel and the menu around it (display preferences' Keyboard shortcuts)
        const menu = host.closest("details");
        if (!menu) return;
        event.preventDefault();
        for (let d = menu; d; d = d.parentElement?.closest("details")) d.open = true;
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

// ---- sluice-search -----------------------------------------------------------------------------
// A list found as one types: its field hides the items it holds (`[data-find]`, their words)
// that do not hold every word typed, a group (`[data-find-group]`) with none left, and says how
// many match in its `[data-find-status]`; Escape clears it.
define("sluice-search", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "list", description: "Its input[type=search], the [data-find] items, [data-find-group]s and [data-find-status]." }],
    events: [],
  },
  setup({ host, cleanup }) {
    filter(host, listening(cleanup), cleanup);
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
    // the page's stream is what main opens and what reopens it on new signals (the plan's find,
    // Mark as seen: `data-reopens-stream`); a reopening ends the request before it on purpose
    // (Datastar's requestCancellation), so the stream stopped only when none is left open
    const opens = (el) => el === document.querySelector("main#content") || el?.hasAttribute?.("data-reopens-stream");
    let phase = "live", open = 0;
    const show = (next) => {
      phase = next;
      host.hidden = next === "live";
      // the band's live line reads this: "Live" only while the stream is
      root.dataset.stream = next;
      const words = host.querySelector(".stream-words");
      if (next === "paused") words.textContent = "Updates paused. Reconnecting…";
      if (next === "stopped") words.textContent = `Updates stopped at ${clock(new Date())}.`;
    };
    on(document, "datastar-fetch", (event) => {
      if (!opens(event.detail.el)) return;
      const type = event.detail.type;
      if (type === "started") {
        open++;
      } else if (type === "finished") {
        open = Math.max(0, open - 1);
        setTimeout(() => { if (!open) show("stopped"); });
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
    // Reconnect is the server's `@get` of the page's stream (`.stream-retry`)
    const reconnect = () => { show("paused"); host.querySelector(".stream-retry")?.click(); };
    const revive = () => { if (phase === "stopped" && !document.hidden) reconnect(); };
    on(window, "online", revive);
    on(window, "focus", revive);
    on(document, "visibilitychange", revive);
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

// ---- sluice-steps ------------------------------------------------------------------------------
// A unit's step view (`unit_steps`): each `li.sv-row` a step (`data-sv` its id), its `a.sv-a` its
// link (with `data-step` the drawer opens it). A click anywhere on a row follows its link, the
// arrows, Home and End move between the rows' links, and the row in focus, clicked, or open in
// the drawer (`#step:<id>`) is the one selected: the host's `selected`, kept through a patch.
// ---- sluice-day --------------------------------------------------------------------------------
// The day line (/day): the server lays its hours out and draws its rule at now and each running
// run's bar to it as it read the clock; each tick of the page's clock moves them on (the
// stream's version leaves the clock out). `data-from` and `data-to` are its first and last
// instant (seconds); a running bar's `data-open` is when its run started.
define("sluice-day", {
  props: () => ({}),
  manifest: {
    slots: [{ name: "ticks", description: ".dl-ticks: the hours and the now rule." },
            { name: "rows", description: ".dl-row a project: its name and its track of run bars (a.dl-bar)." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    const toNow = (now = Date.now()) => {
      const from = Number(host.dataset.from) * 1000, to = Number(host.dataset.to) * 1000;
      if (!(to > from)) return;
      const at = (t) => Math.min(100, Math.max(0, (t - from) / (to - from) * 100));
      const here = `${at(now).toFixed(3)}%`;
      if (host.style.getPropertyValue("--now") !== here) host.style.setProperty("--now", here);
      for (const bar of host.querySelectorAll(".dl-bar[data-open]")) {
        const width = `${Math.max(0.15, at(now) - at(Number(bar.dataset.open) * 1000)).toFixed(3)}%`;
        if (bar.style.width !== width) bar.style.width = width;
      }
    };
    on(window, "sluice-tick", (event) => toNow(event.detail));
    toNow();
  },
});

define("sluice-steps", {
  props: ({ string }) => ({ selected: string }),
  manifest: {
    slots: [{ name: "rows", description: "ol.sv-rows: an li.sv-row a step (data-sv its id), its a.sv-a the step's link." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const on = listening(cleanup);
    // its "now" line and each running run's bar to it: the server draws the axis to now
    // rounded up, so it redraws once a step and never with the clock; `data-now` on .sv-g is
    // the axis's last stretch, its start (seconds), its place and how far a second moves along
    // it (percent), from which each tick of the page's clock keeps them at the reader's now
    const toNow = (now = Date.now()) => {
      const view = host.querySelector(".sv-g[data-now]");
      if (!view) return;
      const [from, place, rate] = view.dataset.now.split(" ").map(Number);
      if (![from, place, rate].every(Number.isFinite)) return;
      const at = Math.min(100, Math.max(0, place + (now / 1000 - from) * rate));
      const here = `${at.toFixed(3)}%`;
      if (view.style.getPropertyValue("--now") !== here) view.style.setProperty("--now", here);
      for (const bar of view.querySelectorAll(".sv-b[data-open]")) {
        const width = `${Math.max(0, at - Number(bar.dataset.open)).toFixed(3)}%`;
        if (bar.style.getPropertyValue("--w") !== width) bar.style.setProperty("--w", width);
      }
    };
    on(window, "sluice-tick", (event) => toNow(event.detail));
    toNow();
    const rows = () => [...host.querySelectorAll("li.sv-row")];
    const mark = () => {
      const id = host.getAttribute("selected") ?? "";
      for (const row of rows()) {
        const chosen = row.dataset.sv === id;
        if (row.classList.contains("is-sel") !== chosen) row.classList.toggle("is-sel", chosen);
      }
    };
    const choose = (id) => {
      if ((host.getAttribute("selected") ?? "") === id) return;
      if (id) host.setAttribute("selected", id); else host.removeAttribute("selected");
      mark();
    };
    on(host, "click", (event) => {
      const row = event.target.closest?.("li.sv-row");
      if (!row || !host.contains(row)) return;
      choose(row.dataset.sv);
      if (!plainClick(event) || event.target.closest("a, button, summary, input, textarea")) return;
      if (getSelection && !getSelection().isCollapsed) return;
      row.querySelector("a.sv-a")?.click();
    });
    on(host, "focusin", (event) => {
      const row = event.target.closest?.("li.sv-row");
      if (row && host.contains(row)) choose(row.dataset.sv);
    });
    on(host, "keydown", (event) => {
      if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
      const link = event.target.closest?.("a.sv-a");
      if (!link || !host.contains(link)) return;
      const links = rows().map((row) => row.querySelector("a.sv-a")).filter(Boolean);
      const at = links.indexOf(link);
      const next = { ArrowDown: links[at + 1], ArrowUp: links[at - 1], Home: links[0], End: links.at(-1) }[event.key];
      if (!next) return;
      event.preventDefault();
      next.focus();
    });
    // the step open in the drawer is the one selected
    const follow = () => {
      let id = "";
      try { id = location.hash.startsWith("#step:") ? decodeURIComponent(location.hash.slice(6)) : ""; } catch { /* a bad hash */ }
      if (id && rows().some((row) => row.dataset.sv === id)) choose(id);
    };
    on(window, "hashchange", follow);
    const observer = new MutationObserver(mark);
    observer.observe(host, { childList: true, subtree: true });
    cleanup(() => observer.disconnect());
    follow();
    mark();
  },
});
