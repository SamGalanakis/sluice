// The inbox's OpenUI renderer (SPEC §8). An open item's `ui` is an OpenUI Lang program: it is
// parsed by @openuidev/lang-core (from the CDN, pinned) against the closed vocabulary in
// openui.json and drawn here with plain DOM (text only, never HTML from the program). A line
// the parser cannot use is dropped and counted in a visible note. A Button answers the item
// with {action, params, values}, POSTed as JSON to the same route the no-JS text box posts to.
import { createParser, parseRules, validate } from
  "/static/lang-core-0.3.0.js";

const VOCAB = await (await fetch("/static/openui.json")).json();

/** A vocabulary type ("string[]", "\"a\" | \"b\"", "Component[]") as JSON Schema, which the
 * parser checks prop values against. */
function schemaOf(type) {
  type = type.trim();
  if (type.endsWith("[]")) return { type: "array", items: schemaOf(type.slice(0, -2)) };
  if (type.startsWith('"')) return { enum: type.split("|").map(s => JSON.parse(s.trim())) };
  return { string: { type: "string" }, number: { type: "number" }, boolean: { type: "boolean" },
           "Record<string, any>": { type: "object" } }[type] ?? {};
}

const SCHEMA = { $defs: Object.fromEntries(VOCAB.components.map(c => [c.name, {
  properties: Object.fromEntries(c.props.map(([n, t]) => [n.replace(/\?$/, ""), schemaOf(t)])),
  required: c.props.filter(([n]) => !n.endsWith("?")).map(([n]) => n),
}])) };
const parser = createParser(SCHEMA, VOCAB.root);

function h(tag, attrs = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v !== undefined && v !== null && v !== false) el.setAttribute(k, v === true ? "" : v);
  }
  el.append(...kids.flat(Infinity).filter(k => k !== undefined && k !== null && k !== false)
    .map(k => (k instanceof Node ? k : String(k))));
  return el;
}

/** One labelled field: registers how to read its value and check its rules. */
function field(ctx, p, kind, control, read) {
  const error = h("span", { class: "ou-error", hidden: true });
  ctx.fields.push({ form: ctx.form, name: p.name, rules: parseRules(p.rules), read, error });
  const label = p.label && kind !== "Checkbox" ? h("span", { class: "ou-label" }, p.label) : null;
  return h(kind === "Radio" ? "fieldset" : "label", { class: "ou-field" },
           kind === "Radio" && p.label ? h("legend", {}, p.label) : label, control, error);
}

const RENDERERS = {
  Stack: (p, ctx) => h("div", { class: `ou-stack ou-${p.direction === "row" ? "row" : "col"}` },
                       draw(p.children, ctx)),
  Heading: p => h(`h${Math.min(3, Math.max(1, Math.round(p.level ?? 2))) + 2}`, {}, p.text),
  Text: p => h("p", { class: p.tone === "muted" ? "muted" : null }, p.text),
  Callout: p => h("div", { class: `ou-callout ou-${p.variant ?? "info"}`, role: "note" },
                  p.title ? h("b", {}, p.title) : null, h("div", {}, p.text)),
  Table: p => h("div", { class: "scroll" }, h("table", {},
    p.caption ? h("caption", {}, p.caption) : null,
    h("tr", {}, p.columns.map(c => h("th", {}, c))),
    p.rows.map(r => h("tr", {}, (Array.isArray(r) ? r : [r]).map(c => h("td", {}, c)))))),
  Separator: () => h("hr"),
  Form: (p, ctx) => {
    const inner = { ...ctx, form: p.name };
    return h("div", { class: "ou-form" }, draw(p.fields, inner),
             h("div", { class: "ou-stack ou-row" }, draw(p.buttons, inner)));
  },
  Input: (p, ctx) => {
    const el = h("input", { name: p.name, type: p.type ?? "text", placeholder: p.placeholder,
                            value: p.value });
    return field(ctx, p, "Input", el, () => {
      const v = el.value;
      return p.type === "number" && v.trim() !== "" && !Number.isNaN(Number(v)) ? Number(v) : v;
    });
  },
  Textarea: (p, ctx) => {
    const el = h("textarea", { name: p.name, rows: p.rows ?? 3, placeholder: p.placeholder },
                 p.value ?? "");
    return field(ctx, p, "Textarea", el, () => el.value);
  },
  Select: (p, ctx) => {
    const el = h("select", { name: p.name }, h("option", { value: "" }, "—"),
                 p.options.map(o => h("option", { value: o, selected: o === p.value }, o)));
    return field(ctx, p, "Select", el, () => el.value);
  },
  Radio: (p, ctx) => {
    const group = `${ctx.key}/${ctx.form ?? ""}/${p.name}`;
    const boxes = p.options.map(o => h("input", { type: "radio", name: group, value: o,
                                                  checked: o === p.value }));
    const el = h("div", { class: "ou-stack ou-col" },
                 boxes.map((b, i) => h("label", {}, b, " ", p.options[i])));
    return field(ctx, p, "Radio", el, () => boxes.find(b => b.checked)?.value ?? "");
  },
  Checkbox: (p, ctx) => {
    const el = h("input", { type: "checkbox", name: p.name, checked: p.checked === true });
    return field(ctx, p, "Checkbox", h("span", {}, el, " ", p.label), () => el.checked);
  },
  Button: (p, ctx) => {
    ctx.buttons.push(p.label);
    const b = h("button", { type: "button",
                            class: p.variant === "secondary" ? "secondary" : "primary" }, p.label);
    b.addEventListener("click", () => submit(ctx, p, ctx.form));
    return b;
  },
};

/** Draw a parsed value: text, a list of values, or an element of the vocabulary. */
function draw(value, ctx) {
  if (value === null || value === undefined || value === false) return null;
  if (Array.isArray(value)) return value.map(v => draw(v, ctx));
  if (typeof value !== "object") return String(value);
  const render = value.type === "element" ? RENDERERS[value.typeName] : undefined;
  if (!render) return null;
  try {
    return render(value.props ?? {}, ctx);
  } catch (err) {  // one broken component must not take the rest with it
    ctx.dropped.push(`${value.statementId ?? value.typeName}: could not be drawn (${err})`);
    return null;
  }
}

/** Top-level lines that are neither a statement (`name = ...`), a comment nor a fence: the
 * parser skips them silently, so they are counted here. */
function strayLines(src) {
  const out = [];
  let depth = 0, quote = null;
  for (const line of src.split("\n")) {
    const t = line.trim();
    if (depth === 0 && t && !/^[A-Za-z_$][\w$]*\s*=/.test(t) && !/^(\/\/|#|```)/.test(t)) {
      out.push(`${t.slice(0, 60)}: not a statement`);
    }
    for (const c of line) {
      if (quote) { if (c === quote) quote = null; continue; }
      if (c === '"' || c === "'") quote = c;
      else if ("([{".includes(c)) depth++;
      else if (")]}".includes(c)) depth = Math.max(0, depth - 1);
    }
    quote = null;
  }
  return out;
}

async function send(area, answer) {
  const note = area.querySelector(".ou-status") ?? area.appendChild(h("p", { class: "ou-status" }));
  area.querySelectorAll("button").forEach(b => { b.disabled = true; });
  try {
    const res = await fetch(area.dataset.url, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(answer),
    });
    const got = await res.json();
    if (res.ok) {
      area.replaceChildren(h("p", { class: "ou-status" }, "Answered."));
      answered(area);
      return;
    }
    note.textContent = [got.message ?? `could not answer (${res.status})`,
                        ...(got.errors ?? [])].join(" — ");
    if (res.status !== 409) area.querySelectorAll("button").forEach(b => { b.disabled = false; });
  } catch (err) {
    note.textContent = `could not answer: ${err}`;
    area.querySelectorAll("button").forEach(b => { b.disabled = false; });
  }
}

/** The server took the answer: show it now, whether or not the page's stream is connected (it
 * may be reconnecting after a restart). On the open view the item leaves the list, and the
 * nav's count drops; the stream's next patch says the same. */
function answered(area) {
  const view = new URLSearchParams(location.search).get("status") ?? "open";
  const list = area.closest("#inbox-items");
  if (view === "open" && list) {
    area.closest("article.item")?.remove();
    if (!list.querySelector("article.item")) {
      list.prepend(h("p", { class: "empty" }, "Nothing is waiting on you."));
    }
  }
  const badge = document.querySelector("#nav-inbox .badge");
  const left = badge ? Number(badge.textContent) - 1 : NaN;
  if (left > 0) badge.textContent = String(left);
  else if (badge) badge.remove();
}

function submit(ctx, p, form) {
  const fields = ctx.fields.filter(f => f.form === form);
  let ok = true;
  if (p.variant !== "secondary") {
    for (const f of fields) {
      const message = validate(f.read(), f.rules);
      f.error.hidden = !message;
      f.error.textContent = message ?? "";
      if (message) ok = false;
    }
  }
  if (!ok) return;
  const values = Object.fromEntries(fields.map(f => [f.name, f.read()]));
  send(ctx.area, { action: p.action ?? "submit", params: p.params ?? {}, values });
}

/** Render one item's answer area: its program above the text box, which stays (folded away
 * when the program has buttons) so there is always a way to answer. */
function render(area) {
  if (area.dataset.drawn) return;
  area.dataset.drawn = "1";
  const box = area.querySelector("form");
  box?.addEventListener("submit", ev => {
    ev.preventDefault();
    send(area, { action: "answer", text: new FormData(box).get("text") ?? "" });
  });
  const src = area.dataset.ui;
  if (!src) return;
  const ctx = { area, key: area.dataset.key, form: undefined, fields: [], buttons: [], dropped: [] };
  let result;
  try {
    result = parser.parse(src);
  } catch (err) {
    result = { root: null, meta: { errors: [{ message: String(err) }], unresolved: [], orphaned: [] } };
  }
  const drawn = draw(result.root, ctx);
  const m = result.meta;
  const dropped = [
    ...m.errors.map(e => `${e.statementId ?? e.component ?? "?"}: ${e.message}`),
    ...m.unresolved.map(n => `${n}: used but never defined`),
    ...(m.orphaned ?? []).map(n => `${n}: defined but never used`),
    ...strayLines(src), ...ctx.dropped];
  const ui = h("div", { class: "ou-root" }, drawn);
  if (!drawn) ui.append(h("p", { class: "muted" }, "The ui of this item could not be drawn."));
  if (dropped.length) {
    ui.append(h("details", { class: "ou-dropped" },
      h("summary", {}, `${dropped.length} ${dropped.length === 1 ? "line" : "lines"} dropped`),
      h("ul", {}, dropped.map(d => h("li", {}, d)))));
  }
  area.prepend(ui);
  if (box && ctx.buttons.length) {
    const fold = h("details", { class: "ou-words" }, h("summary", {}, "Answer in words instead"));
    box.replaceWith(fold);
    fold.append(box);
  }
}

const drawAll = root => root.querySelectorAll?.(".answer[data-url]").forEach(render);
drawAll(document);
new MutationObserver(changes => changes.forEach(c => c.addedNodes.forEach(n => {
  if (n.nodeType !== 1) return;
  if (n.matches(".answer[data-url]")) render(n);
  drawAll(n);
}))).observe(document.body, { childList: true, subtree: true });

// For the vocabulary test: every component has a renderer and nothing else does.
window.sluiceOpenUI = { components: Object.keys(RENDERERS), vocabulary: VOCAB.components.map(c => c.name) };
