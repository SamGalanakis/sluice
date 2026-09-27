// The nav's project switcher is a <details>: it opens and works without this. This closes it
// on a click elsewhere or Escape, as a menu does.
const open = () => document.querySelectorAll("details.switcher[open]");
document.addEventListener("click", (ev) => {
  for (const d of open()) if (!d.contains(ev.target)) d.open = false;
});
document.addEventListener("keydown", (ev) => {
  if (ev.key !== "Escape") return;
  for (const d of open()) {
    d.open = false;
    d.querySelector("summary").focus();
  }
});
