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

// what the plan shows of a step: the first shown link that opens it (its item in Needs you, its
// row's step line once the row is open)
const onPlan = (sid) => sid && $$("#plan-pane a[data-step]").find((a) => a.dataset.step === sid && shown(a));

const PHONE = matchMedia("(max-width: 720px)");  // the drawer a sheet
const OVER = matchMedia("(max-width: 1199px)");  // the drawer over the page, a modal dialog
// not inside a folded box (a closed <details> hides its content)
const shown = (n) => (n.checkVisibility ? n.checkVisibility() : n.getClientRects().length > 0);

// ---- <sluice-drawer> --------------------------------------------------------------------------
// The step open on the board: `#step:<id>` opens it beside the board (over it below 1200px, a
// modal dialog then, a sheet on a phone), streams its page into `#step-detail` from `base`, and
// closes on Escape, its close button, the scrim or a click on the page around the board. Its tab
// is the page's `tab` signal (the step's stream draws it chosen); each step opens on its
// Overview. `[` and `]` move to the step before or after. The open step is the address's hash, a
// link like any other (DESIGN.md, The drawer).
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
    let opener = null, last = "";
    let focusFrame = 0, scrollTimer = 0;
    let pinned = true;  // the log follows its newest line until the reader scrolls up
    const listeners = new AbortController();
    const on = (target, type, fn, options = {}) => target.addEventListener(type, fn, { ...options, signal: listeners.signal });
    mergePatch({ step: "", sver: "" });
    // closing is going back to the plan's own address, as the browser's Back would
    const close = () => {
      if (!location.hash.startsWith("#step:")) return;
      history.pushState(null, "", location.pathname + location.search);
      window.dispatchEvent(new HashChangeEvent("hashchange"));
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
      if (!sid) {
        // its binding gone, Datastar ends its stream (requestCancellation 'cleanup')
        $("#drawer-stream").replaceChildren();
        mergePatch({ step: "", sver: "" });
        // back to what opened it
        opener?.focus({ preventScroll: true });
        opener = null;
        last = "";
        return;
      }
      const changed = last !== sid;
      last = sid;
      if (changed) {
        // a step opens on its Overview (a failure leads there): the tab the last one was on is
        // that step's, not this one's
        mergePatch({ step: sid, sver: "", tab: "" });
        // the drawer's stream: its binding replaced, the step's before it ends with it
        // (https://data-star.dev/reference/actions#get, requestCancellation 'cleanup')
        const binding = document.createElement("span");
        binding.dataset.init = `@get('${props.base}/steps/${encodeURIComponent(sid)}/stream', {retry: 'always', retryMaxCount: 10, retryMaxWait: 30000, requestCancellation: 'cleanup'})`;
        $("#drawer-stream").replaceChildren(binding);
      }
      const detail = $("#step-detail", drawer);
      if (detail && detail.dataset.step !== sid) {
        detail.replaceChildren();  // no stale detail while the new one streams in
        detail.dataset.step = sid;
        pinned = true;  // a new step's log starts following again
      }
      focusFrame = requestAnimationFrame(() => drawer.focus({ preventScroll: true }));
      // once the page has made room, bring what the plan shows of it into view beside the drawer
      if (!PHONE.matches) {
        scrollTimer = setTimeout(() => onPlan(sid)
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
      if (evt.key === "Escape" && location.hash.startsWith("#step:")) close();
    };
    // `[` and `]` open the step before or after this one in the plan's order as drawn (Needs
    // you, then each row's steps, folded or not): each step once, where it is first linked;
    // never while typing, with a modifier or under an open dialog
    const step = (evt) => {
      const sid = currentStep();
      if (!sid || (evt.key !== "[" && evt.key !== "]") || evt.defaultPrevented
          || evt.ctrlKey || evt.metaKey || evt.altKey || document.querySelector("dialog[open]")) return;
      const t = evt.target;
      if (t instanceof Element && t.closest("input, textarea, select, [contenteditable]:not([contenteditable=false])")) return;
      // every link on the plan that opens a step, in page order
      const links = $$("#plan-pane a[data-step]");
      const ids = [...new Set(links.map((a) => a.dataset.step))];
      const at = ids.indexOf(sid);
      const next = ids[evt.key === "]" ? at + 1 : (at < 0 ? ids.length : at) - 1];
      if (!next) return;
      evt.preventDefault();
      opener = links.find((a) => a.dataset.step === next && shown(a)) || opener;
      location.hash = `step:${encodeURIComponent(next)}`;
    };
    // a click on the page around the board (not on a card, a step view's row, a control, the
    // switcher or in the drawer, and not the end of selecting text) closes the drawer as Escape
    // does
    const INTERACTIVE = "a, button, summary, input, select, textarea, label, details.switcher, "
      + ".scrim, [data-step], li.sv-row";
    const away = (evt) => {
      const t = evt.target;
      if (document.querySelector("dialog[open]") || !currentStep() || evt.button !== 0 || !(t instanceof Element)) return;
      if (drawer.contains(t) || t.closest(INTERACTIVE)) return;
      if (getSelection && !getSelection().isCollapsed) return;
      close();
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
    on($(".close", drawer), "click", close);
    on($(".scrim", host), "click", close);
    on(document, "click", click);
    on(document, "click", away);
    on(document, "keydown", escape);
    on(document, "keydown", step);
    on(window, "hashchange", open);
    on(OVER, "change", modal);
    on(drawer, "scroll", scrolled, { capture: true });
    open();
    cleanup(() => {
      listeners.abort();
      follow.disconnect();
      $("#drawer-stream")?.replaceChildren();
      cancelAnimationFrame(focusFrame);
      clearTimeout(scrollTimer);
      document.documentElement.classList.remove("drawer-open");
      for (const el of inertTargets()) el.inert = false;
    });
  },
});
