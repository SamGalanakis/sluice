// The message pages' script: answer forms (drawn by openui.js, loaded once a page has one),
// read marks, closing questions and the thread's message box.

// ---- answers: openui.js draws each answer area and posts it as JSON ---------------------------
let openui = null;
function answers(root) {
  if (!openui && root.querySelector?.(".answer[data-url]")) openui = import("/static/openui.js");
}

// ---- read marks: only when the owner asks ---------------------------------------------------
// A note stays unread until its card's "Mark read" (or the thread page's) or "Mark all read" is
// pressed: scrolling past never marks one, so nothing drops out of the inbox unseen. A mark
// sends the watermark of what the page drew of that thread, never more; the page's stream then
// moves the notes under "Read today", which the server draws from the stored read marks.
// a request that never reached sluice reads in our words, not the browser's ("Failed to fetch")
const unreached = (error) =>
  error instanceof TypeError ? "Sluice did not answer: is it running? Try again in a moment." : error.message;
// read marks that did not go through: one quiet line at the top of the notes, not one under
// every note
const unsent = new Set();
function readFailed(node) {
  unsent.add(node);
  const group = node.closest("section.notes, #messages-view, main") ?? document.body;
  let status = group.querySelector(":scope .read-status");
  if (!status) {
    status = Object.assign(document.createElement("p"), { className: "meta read-status", role: "status" });
    const words = document.createElement("span");
    const retry = Object.assign(document.createElement("button"), { type: "button", className: "link-button", textContent: "Try again" });
    retry.onclick = () => {
      const again = [...unsent];
      unsent.clear();
      status.remove();
      again.forEach(markRead);
    };
    status.append(words, " ", retry);
    const help = group.querySelector(".notes-help");
    if (help) help.after(status); else group.prepend(status);
  }
  status.firstChild.textContent = `${unsent.size === 1 ? "A note was" : `${unsent.size} notes were`} not marked read: sluice did not take it.`;
}
async function markRead(node) {
  if (!node.isConnected || node.disabled) return;
  node.disabled = true;
  try {
    const response = await fetch(node.dataset.readUrl, { method: "POST", headers: { "content-type": "application/json" },
                                                         body: JSON.stringify({ thread: node.dataset.thread, through: Number(node.dataset.through) }) });
    if (!response.ok) throw new Error("Could not mark this note read.");
    node.textContent = "Marked read";
  } catch {
    node.disabled = false;
    readFailed(node);
  }
}
document.addEventListener("click", (event) => {
  const one = event.target.closest?.("button.mark-read");
  if (one) { markRead(one); return; }
  if (event.target.closest?.("button.mark-all")) {
    document.querySelectorAll("section.notes button.mark-read").forEach(markRead);
  }
});
function marks() {
  // the buttons need script: without it they stay hidden
  document.querySelectorAll("button.mark-read[hidden], button.mark-all[hidden]").forEach((b) => { b.hidden = false; });
}

// ---- Answer: opens the box under the buttons, which keep their places --------------------------
document.addEventListener("click", (event) => {
  const toggle = event.target.closest?.("button.q-toggle");
  if (!toggle) return;
  const box = document.getElementById(toggle.getAttribute("aria-controls"));
  if (!box) return;
  const open = !box.hasAttribute("data-open");
  box.toggleAttribute("data-open", open);
  toggle.setAttribute("aria-expanded", String(open));
  if (open) box.querySelector("textarea, input, select, button")?.focus({ preventScroll: true });
});

// ---- closing a question: at once, without leaving the page --------------------------------
document.addEventListener("submit", async (event) => {
  const form = event.target;
  if (!form.matches?.("form.q-close")) return;
  event.preventDefault();
  const button = form.querySelector("button");
  button.disabled = true;
  try {
    const response = await fetch(form.action, { method: "POST", headers: { "content-type": "application/json" },
                                                body: JSON.stringify({ body: "", answer: { action: "close" } }) });
    if (!response.ok) throw new Error((await response.json().catch(() => ({}))).message ?? "Sluice did not close it.");
    form.closest("li, article.item")?.classList.add("closing");
    form.replaceChildren(Object.assign(document.createElement("span"), { className: "meta", textContent: "Closed." }));
  } catch (error) {
    button.disabled = false;
    form.querySelector(".ou-status")?.remove();
    form.append(Object.assign(document.createElement("span"), { className: "ou-status", role: "status", textContent: ` ${unreached(error)}` }));
  }
});

// ---- the thread's message box --------------------------------------------------------------
function replies(root) {
  root.querySelectorAll?.(".thread-reply").forEach((form) => {
    if (form.dataset.bound) return;
    form.dataset.bound = "1";
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const status = form.querySelector(".ou-status");
      const data = new FormData(form);
      try {
        const response = await fetch(form.action, { method: "POST", headers: { "content-type": "application/json" },
                                                     body: JSON.stringify({ body: data.get("body"), to: data.get("to"), ask: data.get("ask") === "true" }) });
        const result = await response.json().catch(() => ({}));
        if (!response.ok) throw new Error(result.message ?? "Sluice did not take the message.");
        status.textContent = "Sent."; form.reset();
      } catch (error) { status.textContent = unreached(error); }
    });
  });
}

function bind(root) {
  answers(root);
  marks();
  replies(root);
}
bind(document);
new MutationObserver(() => bind(document)).observe(document.body, { childList: true, subtree: true });

// ---- a thread opened without an anchor starts at its end: its newest message and the box ----
const replyBox = document.querySelector("#messages-view .thread-reply");
if (replyBox && !location.hash) requestAnimationFrame(() => replyBox.scrollIntoView({ block: "end" }));
