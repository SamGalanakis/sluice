// <sluice-board>: the project page with its board beside the plan (docs("board")). Rust draws
// the host (`#project-board`) and everything in it; this setup only enhances it, all of it
// undone by its cleanup (the splitter is `sluice-splitter`, the description's More and the
// board's document `sluice-fold`: components.js; the find and the board's Buttons are
// Datastar's, in the markup):
// (the view, Plan · Both · Board, is the server's alone: the switch is links, the choice kept
// in the project's cookie, and with none chosen the stylesheet picks by the window's width)
// - the board column's height and its "More below" cue;
// - a step whose state the live plan moves on is said once, politely, in #announce, and its row
//   flashes once where it stands;
// - an answer in place: ⌘↵ sends it, Escape lets it go; Retry with feedback opens its field;
// - after a write from a form with `data-focus-after`, the focus goes to that element once the
//   stream draws it (an item's Undo, the next item).
const runtime = document.querySelector("script[data-datastar-runtime]")?.src;
const { rocket } = await import(runtime);


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

    // ---- the owner's Plan folds ----
    // Morph preserves `open` on surviving disclosures. Session memory also covers a unit
    // moved into a folded group, a removed/re-created fold and a page reload. Datastar cannot
    // preserve an attribute on an element that no longer exists. Only the viewer's choices
    // are kept; the server still supplies each fold's first default.
    const foldPrefix = `sluice:plan-fold:${host.dataset.project}:`;
    const initialized = new WeakSet();
    const foldKey = (d) => {
      if (d.dataset.planFold || d.id) return d.dataset.planFold || d.id;
      const parent = d.parentElement.closest("[id]") || host;
      const peers = [...parent.querySelectorAll("details")].filter(e => e.className === d.className);
      return `${parent.id}:${d.className}:${peers.indexOf(d)}`;
    };
    const restoreFolds = () => {
      for (const d of host.querySelectorAll("#plan-pane details")) {
        if (initialized.has(d)) continue;
        initialized.add(d);
        try {
          const kept = sessionStorage.getItem(foldPrefix + foldKey(d));
          if (kept !== null) d.open = kept === "open";
        } catch { /* session storage may be unavailable */ }
      }
    };
    on(host, "toggle", (event) => {
      const d = event.target;
      if (!(d instanceof HTMLDetailsElement) || !d.closest("#plan-pane") || !host.contains(d)) return;
      try { sessionStorage.setItem(foldPrefix + foldKey(d), d.open ? "open" : "closed"); }
      catch { /* preserve attributes still work without storage */ }
    }, { capture: true });
    restoreFolds();
    observe(host, restoreFolds, { childList: true, subtree: true });

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

    // ---- a row whose steps moved on flashes once, where it stands ----
    // Its `data-said` (each step's state) changed under a patch: the row's wash goes teal and
    // fades over 1.4s (style.css, `--flash`). It never moves: the order is the viewer's.
    const flash = (row) => {
      row.removeAttribute("data-flash");
      void row.offsetWidth;  // a second change restarts it
      row.setAttribute("data-flash", "");
      const end = () => row.removeAttribute("data-flash");
      row.addEventListener("animationend", end, { once: true });
      setTimeout(end, 1600);
    };

    // a patch may redraw the column without the height it was given, or move a step on
    observe(host, (records) => {
      fit();
      for (const r of records) {
        if (r.attributeName === "data-said" && r.oldValue !== null && r.oldValue !== r.target.dataset.said
          && r.target.matches(".ro-u")) flash(r.target);
      }
      if (records.some((r) => r.type === "childList" || r.attributeName === "data-said" || r.attributeName === "data-state")) say();
    }, { childList: true, subtree: true, attributes: true, attributeOldValue: true, attributeFilter: ["style", "data-said", "data-state"] });

    // ---- an answer in place ----
    // ⌘↵ (Ctrl+Enter) sends what was written; Escape lets it go and gives the focus back. Retry
    // with feedback opens its field under the failure, and Escape folds it again.
    on(host, "keydown", (event) => {
      const field = event.target.closest?.("textarea[data-answer]");
      if (!field) return;
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        if (field.value.trim()) field.form?.requestSubmit();
      } else if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        field.value = "";
        const item = field.closest(".nd-i"), toggle = item?.querySelector("[data-fb]");
        if (field.form?.classList.contains("fb") && toggle) {
          item.classList.remove("fb-open");
          toggle.setAttribute("aria-expanded", "false");
          toggle.focus();
        } else {
          field.blur();
        }
      }
    });
    on(host, "click", (event) => {
      const toggle = event.target.closest?.("[data-fb]");
      const item = toggle?.closest(".nd-i");
      if (!item) return;
      const open = !item.classList.contains("fb-open");
      item.classList.toggle("fb-open", open);
      toggle.setAttribute("aria-expanded", String(open));
      if (open) item.querySelector(".answer-in.fb textarea")?.focus();
    });
    fitPane();
  },
});
