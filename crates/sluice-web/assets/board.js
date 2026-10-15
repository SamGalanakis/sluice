// <sluice-board>: the project page with its board beside the plan (docs("board")). Rust draws
// the host (`#project-board`) and everything in it; this setup only enhances it, all of it
// undone by its cleanup (the splitter is `sluice-splitter`, the description's More and the
// board's document `sluice-fold`: components.js; the find and the board's Buttons are
// Datastar's, in the markup):
// - the view switch: Plan · Both · Board from 1280px, Plan · Board below; a click shows the
//   view at once and keeps it per project (localStorage, wide and narrow apart, and the cookie
//   for the server's first paint); until one is picked the server's choice holds, else a phone
//   shows the board (its live lanes are the quick check) unless the project needs attention (a
//   failure, a cancel, a quiet run or a pause: `data-attention`), when it shows the plan, which
//   leads with what stopped; the window between a phone and 1280px shows the plan. The view is
//   the host's `data-view`, which a patch leaves as it is (`data-preserve-attr`);
// - the board column's height and its "More below" cue;
// - a step whose state the live plan moves on is said once, politely, in #announce;
// - after a write from a form with `data-focus-after`, the focus goes to that element once the
//   stream draws it (an item's Undo, the next item).
const runtime = document.querySelector("script[data-datastar-runtime]")?.src;
const { rocket } = await import(runtime);

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
const mode = () => (WIDE.matches ? "wide" : "narrow");
const viewKey = (m, id) => (m === "wide" ? `sluice.view-wide.${id}` : `sluice.view.${id}`);
// an address that asks for a search, a Show, an Order, a step's chain or a recipe's units is
// about the plan
const planAsked = () => {
  const q = new URLSearchParams(location.search);
  return ["q", "show", "order", "root", "recipe"].some((k) => q.get(k));
};
const plainClick = (event) => event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey;

rocket("sluice-board", {
  mode: "light",
  renderOnPropChange: false,
  props: () => ({}),
  manifest: {
    slots: [{ name: "plan", description: "#plan-pane: the plan." },
            { name: "board", description: ".board-side: the splitter and #board-pane, the board's column, with its More below cue." }],
    events: [],
  },
  setup({ host, cleanup }) {
    const done = new AbortController();
    const on = (target, type, fn, options = {}) => target.addEventListener(type, fn, { ...options, signal: done.signal });
    const observers = [];
    const frames = new Set();
    cleanup(() => {
      done.abort();
      for (const o of observers) o.disconnect();
      for (const f of frames) cancelAnimationFrame(f);
    });
    const observe = (target, fn, options) => {
      const o = new MutationObserver(fn);
      o.observe(target, options);
      observers.push(o);
    };
    // one call of `fn` in the next frame, however often it is asked for
    const once = (fn) => {
      let frame = 0;
      return () => {
        if (frame) return;
        frame = requestAnimationFrame(() => { frames.delete(frame); frame = 0; fn(); });
        frames.add(frame);
      };
    };

    // ---- the view: Plan · Both · Board ----
    const chosen = {};  // this tab's choice, should storage refuse it
    const served = host.dataset.view;  // the server's: `?view=` or the project's cookie
    const viewOf = () => {
      if (!host.classList.contains("has-panel")) return "plan";
      const m = mode();
      const views = m === "wide" ? ["plan", "both", "board"] : ["plan", "board"];
      const v = chosen[m] ?? store.get(viewKey(m, host.dataset.project)) ?? served;
      if (!(m in chosen) && planAsked()) return m === "wide" && v === "plan" ? "plan" : m === "wide" ? "both" : "plan";
      if (views.includes(v)) return v;
      if (m === "wide") return "both";
      return PHONE.matches && !host.hasAttribute("data-attention") ? "board" : "plan";
    };
    // the page in its view and the switch (in the page's row, under its name) in step with it
    const sync = () => {
      const view = viewOf();
      if (host.dataset.view !== view) host.dataset.view = view;
      for (const tab of document.querySelectorAll("[data-view-tab]")) {
        const current = String(tab.dataset.viewTab === view);
        if (tab.getAttribute("aria-current") !== current) tab.setAttribute("aria-current", current);
      }
    };
    on(document, "click", (event) => {
      const tab = event.target.closest?.("[data-view-tab]");
      if (!tab || !plainClick(event)) return;
      event.preventDefault();
      const view = tab.dataset.viewTab;
      chosen[mode()] = view;
      store.set(viewKey(mode(), host.dataset.project), view);
      document.cookie = `sluice_view_${host.dataset.project}=${view}; Path=/; Max-Age=34560000; SameSite=Lax`;
      sync();
    });
    on(WIDE, "change", sync);
    on(PHONE, "change", sync);

    // ---- the board column ----
    // It scrolls on its own and is at most the window's height once it sticks; until the page
    // has scrolled it up to its sticky top, it ends where the window does, so its own end (and
    // the shade that says more lies past it) is always in view.
    const pane = () => host.querySelector("#board-pane");
    // While the column scrolls on its own and more of it lies below, a "More below" cue sits at
    // its foot over the shade; it scrolls the column on by most of a screen, and goes at the end.
    const moreBelow = () => {
      const p = pane(), cue = p?.querySelector(".more-below");
      if (!cue) return;
      const scrolls = getComputedStyle(p).overflowY === "auto";
      const hide = !scrolls || p.scrollTop + p.clientHeight >= p.scrollHeight - 24;
      if (cue.hidden !== hide) cue.hidden = hide;
    };
    const fitPane = () => {
      const p = pane();
      if (!p) return;
      const top = Math.max(16, p.getBoundingClientRect().top);
      const height = `${Math.max(240, Math.floor(window.innerHeight - top - 16))}px`;
      if (p.style.getPropertyValue("--pane-max") !== height) p.style.setProperty("--pane-max", height);
      moreBelow();
    };
    const fit = once(fitPane);
    on(window, "resize", fit);
    on(window, "scroll", fit, { passive: true });
    on(host, "scroll", (event) => { if (event.target === pane()) moreBelow(); }, { capture: true, passive: true });
    on(host, "click", (event) => {
      const p = event.target.closest?.(".more-below")?.closest("#board-pane");
      if (p) p.scrollBy({ top: p.clientHeight * 0.8, behavior: "smooth" });
    });

    // ---- the focus after a write ----
    // A form's write ends (Datastar's datastar-fetch "finished") before or after the stream
    // draws what it did; once the write was taken (its notice is no alert), the element named
    // in `data-focus-after` takes the focus as soon as it is there, the page left where it was.
    let focusAfter = null;
    const focusNow = () => {
      if (!focusAfter) return;
      const target = document.getElementById(focusAfter.id);
      if (!target) return;
      const { left, top } = focusAfter;
      focusAfter = null;
      target.focus({ preventScroll: true });
      window.scrollTo({ left, top });
    };
    on(document, "datastar-fetch", (event) => {
      const form = event.detail.el;
      if (event.detail.type !== "finished" || !(form instanceof HTMLFormElement) || !form.dataset.focusAfter) return;
      if (form.querySelector(".form-said[role=alert]")?.textContent) return;
      focusAfter = { id: form.dataset.focusAfter, left: scrollX, top: scrollY, until: Date.now() + 10000 };
      focusNow();
    });
    observe(document.body, () => {
      if (focusAfter && Date.now() > focusAfter.until) focusAfter = null;
      focusNow();
    }, { childList: true, subtree: true });

    // ---- what the live plan moved on, said once ----
    // Each step's state in words, from the data-said its unit's item, row, card or Done line
    // carries ("a-build succeeded|a-review skipped"), so a step is still read when its unit
    // moves on.
    const states = () => {
      const out = new Map();
      for (const el of host.querySelectorAll("#plan-pane [data-said]")) {
        for (const pair of el.dataset.said.split("|")) {
          const at = pair.indexOf(" ");
          if (at > 0 && !out.has(pair.slice(0, at))) out.set(pair.slice(0, at), pair.slice(at + 1));
        }
      }
      return out;
    };
    let known = states();
    const say = once(() => {
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
    });

    // a patch may redraw the column without the height it was given, or move a step on
    observe(host, (records) => {
      fit();
      if (records.some((r) => r.type === "childList" || r.attributeName === "data-said" || r.attributeName === "data-state")) say();
      sync();
    }, { childList: true, subtree: true, attributes: true, attributeFilter: ["style", "data-said", "data-state"] });
    sync();
    fitPane();
  },
});
