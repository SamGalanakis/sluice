// The message pages' script: answer forms (drawn by openui.js, loaded once a page has one),
// read marks, closing questions and the thread's message box.

// ---- answers: openui.js draws each answer area and posts it as JSON ---------------------------
let openui = null;
function answers(root) {
  if (!openui && root.querySelector?.(".answer[data-url]")) openui = import("/static/openui.js");
}

// ---- read marks: a note is read once it has been on screen for 2 s, or on "Mark all read" ----
// A watermark describes only this rendered thread, never the whole inbox. A note marked read
// on the inbox moves under "Read just now" (kept on the page, out of the stream's patches),
// since the stream takes it off the unread list.
const SEEN = 2000;
const sent = new Map();
const timers = new Map();
// a request that never reached sluice reads in our words, not the browser's ("Failed to fetch")
const unreached = (error) =>
  error instanceof TypeError ? "Sluice did not answer: is it running? Try again in a moment." : error.message;
// read marks that did not go through: one quiet line at the top of the notes, not one under
// every note (the mark is the page's own bookkeeping, not something the owner asked for)
const unsent = new Set();
function readFailed(node) {
  unsent.add(node);
  const group = node.closest("section.notes, #thread-view, main") ?? document.body;
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
  if (!node.isConnected) return;
  const key = node.dataset.readUrl + "/" + node.dataset.thread;
  const through = Number(node.dataset.through);
  if ((sent.get(key) ?? 0) >= through) return;
  sent.set(key, through);
  try {
    const response = await fetch(node.dataset.readUrl, { method: "POST", headers: { "content-type": "application/json" },
                                                         body: JSON.stringify({ thread: node.dataset.thread, through }) });
    if (!response.ok) throw new Error("Could not mark this note read.");
    keepRead(node);
  } catch {
    sent.delete(key);
    readFailed(node);
  }
}
function keepRead(node) {
  const fold = document.querySelector("#read-now");
  if (!fold || !node.matches("article.note")) return;
  const copy = node.cloneNode(true);
  copy.removeAttribute("data-read-url");
  copy.querySelectorAll("[id]").forEach((el) => el.removeAttribute("id"));
  fold.querySelector(".threads").append(copy);
  fold.hidden = false;
  fold.querySelector(".n").textContent = String(fold.querySelectorAll("article").length);
}
const seen = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    const node = entry.target;
    // half of it in view, or a screenful of a tall one
    const visible = entry.isIntersecting && (entry.intersectionRatio >= 0.5 || entry.intersectionRect.height >= innerHeight * 0.5);
    if (visible && !timers.has(node)) {
      timers.set(node, setTimeout(() => { timers.delete(node); markRead(node); }, SEEN));
    } else if (!visible && timers.has(node)) {
      clearTimeout(timers.get(node)); timers.delete(node);
    }
  }
}, { threshold: [0, 0.25, 0.5, 0.75, 1] });
const watched = new WeakSet();
function watch(root) {
  root.querySelectorAll?.("[data-read-url]").forEach((node) => {
    if (watched.has(node)) return;
    watched.add(node);
    seen.observe(node);
  });
  const all = document.querySelector(".mark-all");
  if (all && !all.dataset.bound) {
    all.dataset.bound = "1";
    all.hidden = false;
    all.addEventListener("click", () => document.querySelectorAll("article.note[data-read-url]").forEach(markRead));
  }
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
  watch(root);
  replies(root);
}
bind(document);
new MutationObserver(() => bind(document)).observe(document.body, { childList: true, subtree: true });
