// The project's board beside the plan (docs("board")). Below 1280px the page shows one of
// Plan and Board at a time; the switch remembers the choice per project (localStorage). A
// board's Button posts its form to the server, which sends the orchestrator a say; here it is
// sent without leaving the page and the answer shown under the board. Without script both
// sections show, one after the other, and a Button posts the page.
const key = id => `sluice.view.${id}`;
function remembered(id) {
  try { return localStorage.getItem(key(id)); } catch { return null; }
}
function remember(id, view) {
  try { localStorage.setItem(key(id), view); } catch { /* private mode: not remembered */ }
}

/** Put the page in its view (the remembered one at first) and the switch in step with it. */
function sync() {
  const page = document.querySelector("#project-board");
  if (!page) return;
  let view = page.dataset.view || remembered(page.dataset.project) || "plan";
  if (!page.classList.contains("has-panel") || !["plan", "board"].includes(view)) view = "plan";
  if (page.dataset.view !== view) page.dataset.view = view;
  for (const tab of page.querySelectorAll("[data-view-tab]")) {
    const on = String(tab.dataset.viewTab === view);
    if (tab.getAttribute("aria-pressed") !== on) tab.setAttribute("aria-pressed", on);
  }
}

document.addEventListener("click", event => {
  const tab = event.target.closest?.("[data-view-tab]");
  const page = tab?.closest("#project-board");
  if (!page) return;
  page.dataset.view = tab.dataset.viewTab;
  remember(page.dataset.project, tab.dataset.viewTab);
  sync();
});

document.addEventListener("submit", async event => {
  const form = event.target.closest?.("form.board-form");
  if (!form) return;
  event.preventDefault();
  const status = form.querySelector(".board-status");
  const buttons = [...form.querySelectorAll("button[name=button]")];
  for (const note of form.querySelectorAll("[data-error-for]")) {
    note.hidden = true;
    note.textContent = "";
  }
  const body = new URLSearchParams(new FormData(form, event.submitter));
  buttons.forEach(b => { b.disabled = true; });
  status.textContent = "Sending…";
  try {
    const response = await fetch(form.action, {
      method: "POST", headers: { accept: "application/json" }, body,
    });
    const got = await response.json().catch(() => ({}));
    if (response.ok) {
      status.textContent = got.message ?? "Sent.";
      return;
    }
    status.textContent = got.message ?? `Could not send (${response.status}).`;
    for (const raw of got.errors ?? []) {
      let error;
      try { error = JSON.parse(raw); } catch { continue; }
      const note = form.querySelector(`[data-error-for="${CSS.escape(error.field ?? "")}"]`);
      if (note) {
        note.textContent = error.message;
        note.hidden = false;
      }
    }
  } catch (error) {
    status.textContent = `Could not send: ${error.message}`;
  } finally {
    buttons.forEach(b => { b.disabled = false; });
  }
});

sync();
new MutationObserver(sync).observe(document.querySelector("#content") ?? document.body,
                                   { childList: true, subtree: true });
