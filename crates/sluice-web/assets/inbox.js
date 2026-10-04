import { drawAll } from "/static/openui.js";

// A watermark describes only this rendered thread, never the whole inbox.
const sent = new Map();
function advance(root) {
  requestAnimationFrame(() => requestAnimationFrame(() => {
    root.querySelectorAll?.("[data-read-url]").forEach(async node => {
      if (!node.isConnected) return;
      const key = node.dataset.readUrl + "/" + node.dataset.thread;
      const through = Number(node.dataset.through);
      if ((sent.get(key) ?? 0) >= through) return;
      sent.set(key, through);
      try {
        const response = await fetch(node.dataset.readUrl, { method: "POST", headers: {"content-type": "application/json"}, body: JSON.stringify({thread: node.dataset.thread, through}) });
        if (!response.ok) throw new Error("Could not mark the thread read.");
      } catch (error) {
        const status = document.createElement("p"); status.className = "ou-status"; status.role = "status";
        status.textContent = error.message;
        const retry = document.createElement("button"); retry.type = "button"; retry.textContent = "Retry";
        retry.onclick = () => { sent.delete(key); status.remove(); advance(document); }; status.append(" ", retry); node.append(status);
      }
    });
  }));
}
function bind(root) {
  drawAll(root);
  root.querySelectorAll?.(".thread-reply").forEach(form => {
    if (form.dataset.bound) return;
    form.dataset.bound = "1";
    form.addEventListener("submit", async event => {
      event.preventDefault();
      const status = form.querySelector(".ou-status");
      const data = new FormData(form);
      try {
        const response = await fetch(form.action, {method: "POST", headers: {"content-type":"application/json"}, body: JSON.stringify({body:data.get("body"), to:data.get("to"), ask:data.get("ask") === "true"})});
        const result = await response.json();
        if (!response.ok) throw new Error(result.message);
        status.textContent = "Sent."; form.reset();
      } catch(error) { status.textContent = error.message; }
    });
  });
  advance(root);
}
bind(document);
new MutationObserver(() => bind(document)).observe(document.body, {childList:true, subtree:true});

function logFilters(root) {
  root.querySelectorAll?.("details.kinds").forEach(details => {
    if (details.dataset.bound) return;
    details.dataset.bound = "1";
    if (matchMedia("(max-width:720px)").matches) details.open = false;
  });
}
logFilters(document);
new MutationObserver(() => logFilters(document)).observe(document.body,{childList:true,subtree:true});
