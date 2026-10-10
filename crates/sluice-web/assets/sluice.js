// Use the same module instance as the layout stream.
const runtimeUrl = document.querySelector("script[data-datastar-runtime]")?.src;
const { rocket, mergePatch } = await import(runtimeUrl);
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

// ---- the open step ---------------------------------------------------------------------------

function currentStep() {
  try {
    const sid = location.hash.startsWith("#step:") ? decodeURIComponent(location.hash.slice(6)) : "";
    return /^[a-z0-9][a-z0-9_-]*$/.test(sid) ? sid : "";
  } catch { return ""; }
}

function markOpen(sid) {
  // a stage's cell on the plan
  for (const n of $$(".open[id^='n-']")) {
    if (n.id !== `n-${sid}`) n.classList.remove("open");
  }
  const card = sid && document.getElementById(`n-${sid}`);
  if (!card) return;
  if (!card.classList.contains("open")) card.classList.add("open");
  // a folded part of the plan holding it opens to it
  for (let d = card.closest("details:not([open])"); d; d = d.parentElement?.closest("details:not([open])")) {
    d.open = true;
  }
}

const PHONE = matchMedia("(max-width: 720px)");  // the drawer a sheet
const OVER = matchMedia("(max-width: 1199px)");  // the drawer over the page, a modal dialog
// not inside a folded box (a closed <details> hides its content)
const shown = (n) => (n.checkVisibility ? n.checkVisibility() : n.getClientRects().length > 0);

// ---- <sluice-drawer> --------------------------------------------------------------------------
// The step open on the board: `#step:<id>` opens it beside the board (over it below 1200px, a
// modal dialog then, a sheet on a phone), streams its page into `#step-detail` from `base`, and
// closes on Escape, its close button, the scrim or a click on the page around the board. Its tab
// is the page's `tab` signal (the step's stream draws it chosen) and, while a step is open and
// its tab is not Overview, the address's `?tab=` (the board's own query is its filters, which
// leave `tab` alone), so a reload or a shared link opens the step on the same tab; moving on to
// another step opens that one on its Overview. `[` and `]` move to the step before or after.
rocket("sluice-drawer", {
  mode: "light",
  renderOnPropChange: false,
  props: ({ string }) => ({ base: string }),
  manifest: {
    slots: [{ name: "scrim", description: ".scrim behind it below 1200px." },
            { name: "drawer", description: "aside#drawer: its close band, #drawer-stream (the step's stream binding) and #step-detail." }],
    events: [],
  },
  setup({ host, props, cleanup }) {
    const drawer = $("#drawer", host);
    let opener = null, stream = null, last = "";
    let focusFrame = 0, scrollTimer = 0;
    let pinned = true;  // the log follows its newest line until the reader scrolls up
    const listeners = new AbortController();
    const on = (target, type, fn, options = {}) => target.addEventListener(type, fn, { ...options, signal: listeners.signal });
    mergePatch({ step: "", sver: "" });
    // the drawer's stream: each call ends the previous one (Datastar's requestCancellation)
    window.sluiceStream = () => {
      stream?.abort();
      stream = new AbortController();
      return stream;
    };
    window.sluiceClose = () => {
      if (!location.hash.startsWith("#step:")) return;
      history.pushState(null, "", location.pathname + location.search);
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    };
    // the open step's tab in the address: `?tab=` while a step is open on a tab past Overview
    const tabInAddress = (key) => {
      const url = new URL(location.href);
      if (key && key !== "overview" && currentStep()) url.searchParams.set("tab", key);
      else url.searchParams.delete("tab");
      if (url.href !== location.href) history.replaceState(history.state, "", url);
    };
    // below 1200px, over the page (a sheet on a phone), it is a modal dialog: the page behind
    // it is inert; from 1200px it is a region beside the page
    const inertTargets = () => {
      const result = [];
      let branch = host;
      while (branch.parentElement) {
        for (const el of branch.parentElement.children) {
          if (el !== branch && el.id !== "announce" && el.tagName !== "SCRIPT") result.push(el);
        }
        if (branch.parentElement === document.body) break;
        branch = branch.parentElement;
      }
      return result;
    };
    const modal = () => {
      const over = Boolean(currentStep()) && OVER.matches;
      drawer.setAttribute("role", over ? "dialog" : "complementary");
      drawer.toggleAttribute("aria-modal", over);
      if (over) drawer.setAttribute("aria-modal", "true");
      for (const el of inertTargets()) el.inert = over;
    };
    const open = () => {
      cancelAnimationFrame(focusFrame);
      clearTimeout(scrollTimer);
      const sid = currentStep();
      document.documentElement.classList.toggle("drawer-open", Boolean(sid));
      drawer.hidden = !sid;
      $(".scrim", host).hidden = !sid;
      modal();
      markOpen(sid);
      if (!sid) {
        stream?.abort();
        $("#drawer-stream").replaceChildren();
        mergePatch({ step: "", sver: "" });
        tabInAddress("");
        // back to the card (beside the drawer, the page brought it into view); a phone's sheet
        // did not, so a name in a card's waits that opened it takes the focus back
        const card = last && document.getElementById(`n-${last}`);
        const named = PHONE.matches && opener?.matches(".pl-waits a") ? opener : null;
        (named || card || opener)?.focus({ preventScroll: true });
        opener = null;
        last = "";
        return;
      }
      const changed = last !== sid, moved = changed && Boolean(last);
      last = sid;
      if (changed) {
        stream?.abort();
        stream = new AbortController();
        window.sluiceStepController = stream;
        // a step opened from another opens on its Overview (a failure leads there): the tab
        // the last one was on is that step's, not this one's. Opened afresh (a reload, a
        // shared link), it opens on the address's tab.
        if (moved) tabInAddress("");
        const tab = moved ? "" : new URLSearchParams(location.search).get("tab") ?? "";
        mergePatch({ step: sid, sver: "", tab });
        const binding = document.createElement("span");
        binding.dataset.init = `@get('${props.base}/steps/${encodeURIComponent(sid)}/stream', {retry: 'always', retryMaxCount: 10, retryMaxWait: 30000, requestCancellation: window.sluiceStepController})`;
        $("#drawer-stream").replaceChildren(binding);
      }
      const detail = $("#step-detail", drawer);
      if (detail && detail.dataset.step !== sid) {
        detail.replaceChildren();  // no stale detail while the new one streams in
        detail.dataset.step = sid;
        pinned = true;  // a new step's log starts following again
      }
      focusFrame = requestAnimationFrame(() => drawer.focus({ preventScroll: true }));
      // once the page has made room, bring the card into view beside the drawer
      if (!PHONE.matches) {
        scrollTimer = setTimeout(() => document.getElementById(`n-${sid}`)
          ?.scrollIntoView({ block: "nearest", inline: "nearest" }), 220);
      }
    };
    const click = (evt) => {
      const a = evt.target.closest?.("a[data-step], a[data-opens]");  // a card, or a source named in its waits
      if (!a || evt.button !== 0 || evt.metaKey || evt.ctrlKey || evt.shiftKey
          || evt.altKey) return;
      evt.preventDefault();
      if (!drawer.contains(a)) opener = a;
      location.hash = `step:${encodeURIComponent(a.dataset.step || a.dataset.opens)}`;
    };
    const escape = (evt) => {
      if (document.querySelector("dialog[open]")) return;
      if (evt.key === "Tab" && currentStep() && OVER.matches) {
        const items = $$("a[href], button, input, select, textarea, [tabindex='0']", drawer).filter(shown);
        const first = items[0], end = items.at(-1);
        if (evt.shiftKey && (document.activeElement === first || document.activeElement === drawer)) { evt.preventDefault(); end?.focus(); }
        else if (!evt.shiftKey && document.activeElement === end) { evt.preventDefault(); first?.focus(); }
      }
      if (evt.key === "Escape" && location.hash.startsWith("#step:")) window.sluiceClose();
    };
    // `[` and `]` open the step before or after this one in the board's order as drawn (its
    // bands, live first): each step once, as its first card, pill or dot shows it; never while
    // typing, with a modifier or under an open dialog
    const step = (evt) => {
      const sid = currentStep();
      if (!sid || (evt.key !== "[" && evt.key !== "]") || evt.defaultPrevented
          || evt.ctrlKey || evt.metaKey || evt.altKey || document.querySelector("dialog[open]")) return;
      const t = evt.target;
      if (t instanceof Element && t.closest("input, textarea, select, [contenteditable]:not([contenteditable=false])")) return;
      // the plan's stage cells, in page order
      const ids = [...new Set($$("#project-board a.sc-a[data-step]")
        .filter(shown).map((a) => a.dataset.step || a.dataset.opens))];
      const at = ids.indexOf(sid);
      const next = ids[evt.key === "]" ? at + 1 : (at < 0 ? ids.length : at) - 1];
      if (!next) return;
      evt.preventDefault();
      opener = document.getElementById(`n-${next}`) || opener;
      location.hash = `step:${encodeURIComponent(next)}`;
    };
    // a click on the page around the board (not on a card, a control, the switcher or in the
    // drawer, and not the end of selecting text) closes the drawer as Escape does
    const INTERACTIVE = "a, button, summary, input, select, textarea, label, details.switcher, "
      + ".scrim, [data-step]";
    const away = (evt) => {
      const t = evt.target;
      if (document.querySelector("dialog[open]") || !currentStep() || evt.button !== 0 || !(t instanceof Element)) return;
      if (drawer.contains(t) || t.closest(INTERACTIVE)) return;
      if (getSelection && !getSelection().isCollapsed) return;
      window.sluiceClose();
    };
    const scrolled = (evt) => {
      const pre = evt.target;
      // The band owns its scroll shadow; the drawer retains its dialog attributes.
      if (pre === drawer) $(".d-top", drawer)?.classList.toggle("scrolled", drawer.scrollTop > 0);
      if (pre.classList?.contains("tail")) {
        pinned = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
      }
    };
    const follow = new MutationObserver(() => {
      const pre = $("#step-detail pre.tail", drawer);
      if (pre && pinned) pre.scrollTop = pre.scrollHeight;
    });
    follow.observe(drawer, { childList: true, subtree: true, characterData: true });
    const close = () => window.sluiceClose();
    on($(".close", drawer), "click", close);
    on($(".scrim", host), "click", close);
    on(document, "click", click);
    on(document, "click", away);
    on(document, "keydown", escape);
    on(document, "keydown", step);
    on(window, "hashchange", open);
    on(OVER, "change", modal);
    on(drawer, "scroll", scrolled, { capture: true });
    on(drawer, "sluice-tab", (evt) => tabInAddress(evt.detail.key));
    open();
    cleanup(() => {
      listeners.abort();
      delete window.sluiceStepController;
      follow.disconnect();
      stream?.abort();
      cancelAnimationFrame(focusFrame);
      clearTimeout(scrollTimer);
      document.documentElement.classList.remove("drawer-open");
      for (const el of inertTargets()) el.inert = false;
      delete window.sluiceStream;
      delete window.sluiceClose;
    });
  },
});
