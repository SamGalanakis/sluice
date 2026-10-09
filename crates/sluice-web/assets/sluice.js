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
  for (const n of $$(".node.open")) {
    if (n.id !== `n-${sid}`) n.classList.remove("open");
  }
  const card = sid && document.getElementById(`n-${sid}`);
  if (!card) {
    const box = $$("details[data-box-steps]").find((d) => d.dataset.boxSteps.split(" ").includes(sid));
    if (box) box.open = true;
    return;
  }
  if (!card.classList.contains("open")) card.classList.add("open");
  // a finished box opens to it, and the shelf it is on
  for (let d = card.closest("details:not([open])"); d; d = d.parentElement?.closest("details:not([open])")) {
    d.open = true;
  }
}

const PHONE = matchMedia("(max-width: 720px)");  // one card per line, no edges; drawer a sheet
const OVER = matchMedia("(max-width: 1199px)");  // the drawer over the page, a modal dialog
// Opening or closing the drawer reflows the page, which can put a card under a pointer that
// has not moved: that card does not trace until the pointer really moves.
let still = false;
document.addEventListener("pointermove", (evt) => {
  if (!still || !(evt.movementX || evt.movementY)) return;
  still = false;
  const n = evt.target.closest?.(TRACES), host = n?.closest("sluice-board");
  if (host) trace(host, traceKey(n));
}, { passive: true });
// What traces when hovered or focused: a card, or a name in a card's waits (its source).
const TRACES = ".node[data-node], .waits a[data-from]";
const traceKey = (el) => (el.matches(".waits a") ? el.dataset.from : el.dataset.node);
// not inside a folded box (a closed <details> hides its content, which keeps its boxes)
const shown = (n) => (n.checkVisibility ? n.checkVisibility() : n.getClientRects().length > 0);

// ---- <sluice-board> ---------------------------------------------------------------------------
// Each edge leaves the bottom of a card (or a unit's box, for a unit gate) and enters the top of
// the card that comes after it, ending in an arrowhead: the line says "this, then that", in a
// unit or between units. Several edges on one side of a card spread along it, in the order of the cards at
// their other ends. An edge that passes rows of cards on its way runs through the nearest gap
// in each (edges sharing a gap sit side by side), so it never hides behind a card.

const SVG = "http://www.w3.org/2000/svg";
const HEAD_W = 3.5, HEAD_H = 6;  // the arrowhead: a shape of its own, which lights up with its edge
const CLEAR = 7;                 // the least space between an edge and a card it passes
const TILE_CLEAR = 12;           // and the more it keeps from a unit's box: never along its border
const SIDE = 5;                  // between edges sharing a gap
const LEFT = 40;                 // how much nearer a gap on the left must be: bypasses keep right

function svgEl(name, attrs) {
  const el = document.createElementNS(SVG, name);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}

// The rows of cards, top to bottom: cards whose boxes overlap in height share one.
function bands(boxes) {
  const out = [];
  for (const r of [...boxes].sort((a, b) => a.top - b.top)) {
    const last = out[out.length - 1];
    if (last && r.top < last.bottom - 2) {
      last.bottom = Math.max(last.bottom, r.bottom);
      last.spans.push(r.span);
    } else {
      out.push({ top: r.top, bottom: r.bottom, spans: [r.span] });
    }
  }
  return out;
}

// The x to pass a row at: in the free gap nearest `want` (one on the right when it is about as
// near, so the edges that pass a card run together on one side), beside any edge already there.
function passAt(band, want, lo, hi, used, key) {
  const gaps = [];
  let x = lo;
  for (const [l, r] of band.spans.map(([l, r]) => [l - CLEAR, r + CLEAR])
    .sort((a, b) => a[0] - b[0])) {
    if (l > x) gaps.push([x, l]);
    x = Math.max(x, r);
  }
  if (hi > x) gaps.push([x, hi]);
  let best = null;
  for (const [l, r] of gaps) {
    const at = Math.min(Math.max(want, l), r);
    const cost = Math.abs(at - want) + (at < want ? LEFT : 0);
    if (!best || cost < best.cost) best = { at, l, r, cost };
  }
  if (!best) return want;
  const slot = `${key}:${Math.round(best.l)}`;
  const n = used.get(slot) || 0;
  used.set(slot, n + 1);
  const side = best.at <= (best.l + best.r) / 2 ? 1 : -1;  // fan out into the gap
  return Math.min(Math.max(best.at + side * n * SIDE, best.l), best.r);
}

function drawEdges(host, data) {
  const plane = $(".plane", host), svg = $("svg.edges", host);
  if (!plane || !svg) return;
  if (PHONE.matches) { svg.replaceChildren(); return; }
  const box = plane.getBoundingClientRect();
  const boxed = $(".boxes", plane)?.classList.contains("boxed");
  const boxes = $$(".box", plane), rect = new Map();
  const at = (r) => ({ left: r.left - box.left, right: r.right - box.left, top: r.top - box.top,
                       bottom: r.bottom - box.top, width: r.width,
                       span: [r.left - box.left, r.right - box.left] });
  // the units laid out on the board (a box, or a one-step unit's card): a line between units
  // passes them, not just their cards
  const tileEls = $$(".layer > .box", plane).filter(shown);
  const tiles = tileEls.map((t) => at(t.getBoundingClientRect()));
  // A lane matrix is a table: the server draws no line to, from or past one (its waits are words
  // in its rows). Should the page and the server ever disagree (a patch on its way), a line that
  // would touch a matrix is left out rather than drawn through its cells.
  const matrices = $$(".matrix", plane).filter(shown).map((m) => at(m.getBoundingClientRect()));
  // a box's label ("a-12") is passed like a card: a line into the box never strikes it
  const labels = $$(".layer > .box > .box-label", plane).filter(shown)
    .map((l) => ({ ...at(l.getBoundingClientRect()), tile: tileEls.indexOf(l.parentElement) }));
  for (const n of $$("[data-node]", plane)) {
    if (!shown(n) || n.closest(".matrix")) continue;
    const r = at(n.getBoundingClientRect());
    // a one-step unit's title over its card is the card's: a line into it ends above the title
    const title = n.parentElement?.matches(".box.solo.titled") && $(":scope > .solo-title", n.parentElement);
    if (title && shown(title)) r.top = Math.min(r.top, at(title.getBoundingClientRect()).top);
    if (r.width === 0 && r.height === 0) continue;
    rect.set(n.dataset.node, { ...r,
                               box: boxes.indexOf(n.closest(".box")),
                               tile: tileEls.indexOf(n.closest(".layer > .box")) });
  }
  // Route within a unit's box past its cards; between units past every card and every other
  // unit, through the gaps of each row they make between the two ends (see the loop below).
  const rowsOf = new Map();
  const route = (a, b) => {
    const [ra, rb] = [rect.get(a), rect.get(b)];
    const inBox = ra.box === rb.box && ra.box >= 0;
    const key = inBox ? `box:${ra.box}` : `tiles:${ra.tile}:${rb.tile}`;
    if (!rowsOf.has(key)) {
      const cards = [...rect.entries()].filter(([k, r]) => !k.startsWith("u:")
        && (!inBox || r.box === ra.box)).map(([, r]) => r);
      const others = inBox ? [] : tiles.filter((_, i) => i !== ra.tile && i !== rb.tile)
        .map((t) => ({ ...t, span: [t.span[0] - TILE_CLEAR, t.span[1] + TILE_CLEAR] }));
      const marks = inBox ? [] : labels;
      const rows = cards.concat(others, marks);
      let lo = -CLEAR * 2, hi = box.width + CLEAR * 2;
      if (inBox && boxed) {
        const l = boxes[ra.box].getBoundingClientRect();
        lo = l.left - box.left + SIDE;
        hi = l.right - box.left - SIDE;
      }
      rowsOf.set(key, { rows, lo, hi, key });
    }
    return rowsOf.get(key);
  };
  // Of the cards shown (a search may leave a step out), one path a pair (see merge). A line
  // runs down, from the foot of its source to the top of the card that waits, and never past a
  // lane matrix: one that would run up or through a matrix is not drawn, and its words under
  // the waiting card say it instead (the server leaves such a wait to words already; this
  // keeps a page caught between two patches honest).
  const down = (a, b) => {
    const [ra, rb] = [rect.get(a), rect.get(b)];
    return rb.top - 1 > ra.bottom && !matrices.some((m) => m.top < rb.top && m.bottom > ra.bottom);
  };
  const listed = (Array.isArray(data) ? data : []).filter(([a, b]) => rect.has(a) && rect.has(b));
  const kept = new Set(listed.filter(([a, b]) => !down(a, b)).map(([a, b]) =>
    $(`.waits a[data-from="${CSS.escape(a)}"][data-to="${CSS.escape(b)}"]`, plane)?.parentElement));
  for (const words of $$(".waits", plane)) {
    // only a change is written: the board redraws on a class that moves its layout
    if (words.classList.contains("kept") !== kept.has(words)) words.classList.toggle("kept");
  }
  const ends = merge(listed.filter(([a, b]) => down(a, b)));
  const cx = (key) => rect.get(key).left + rect.get(key).width / 2;
  // Where each line passes the rows between its ends, routed once from card centre to card
  // centre on a scratch board: its first pass and its last, so the lines leaving a card are
  // spread along its foot in the order they head off, and those arriving along its top in the
  // order they come in. A bypass on the right leaves and arrives on the right: hooks never cross.
  const passes = new Map(), scratch = new Map();
  const rowsBetween = (a, b) => {
    const y1 = rect.get(a).bottom, tip = rect.get(b).top - 1;
    const { rows: things, lo, hi, key } = route(a, b);
    return { y1, tip, lo, hi, key,
             rows: bands(things.filter((r) => r.top > y1 + 1 && r.bottom < tip - 1)) };
  };
  for (const [index, [a, b]] of ends.entries()) {
    const { y1, tip, lo, hi, key, rows } = rowsBetween(a, b);
    const xa = cx(a), xb = cx(b);
    const xs = rows.map((row) => passAt(row, xa + (xb - xa) * (((row.top + row.bottom) / 2 - y1) / (tip - y1)),
                                        lo, hi, scratch, `${key}:${Math.round(row.top)}`));
    passes.set(index, { first: xs.length ? xs[0] : xb, last: xs.length ? xs[xs.length - 1] : xa });
  }
  const spread = (key, others, toward) => {
    const r = rect.get(key), sorted = [...others].sort((p, q) => toward(p) - toward(q));
    return new Map(sorted.map((o, i) => [o.index, r.left + r.width * (i + 1) / (sorted.length + 1)]));
  };
  const outs = new Map(), ins = new Map();
  for (const [index, [a, b]] of ends.entries()) {
    if (!outs.has(a)) outs.set(a, []);
    if (!ins.has(b)) ins.set(b, []);
    outs.get(a).push({other: b, index});
    ins.get(b).push({other: a, index});
  }
  // Where each edge into a card ends: spread along its top, in the order of their sources.
  const arrive = (key, others) => new Map([...spread(key, others, (o) => passes.get(o.index).last)]
    .map(([index, x]) => [index, { x, y: rect.get(key).top - 1 }]));
  const outX = new Map([...outs].map(([k, v]) => [k, spread(k, v, (o) => passes.get(o.index).first)]));
  const inX = new Map([...ins].map(([k, v]) => [k, arrive(k, v)]));
  const used = new Map(), labelLanes = new Map();
  const wires = svgEl("g", { class: "wires" }), names = svgEl("g", { class: "names" });
  const f = (n) => n.toFixed(1);
  for (const [index, [a, b, label, kinds]] of ends.entries()) {
    const x1 = outX.get(a).get(index), y1 = rect.get(a).bottom;
    const end = inX.get(b).get(index), x2 = end.x;
    const tip = rect.get(b).top - 1;
    const y2 = end.y - HEAD_H;  // straight down, into the head
    const pts = [[x1, y1]];
    // the rows of what lies wholly between the two ends: a card under the source in its own
    // box counts, a unit beside the source (begun above it) does not
    const { lo, hi, key, rows } = rowsBetween(a, b);
    rows.forEach((row) => {
      const t = ((row.top + row.bottom) / 2 - y1) / (tip - y1);
      const x = passAt(row, x1 + (x2 - x1) * t, lo, hi, used, `${key}:${Math.round(row.top)}`);
      pts.push([x, row.top - 4], [x, row.bottom + 4]);
    });
    pts.push([x2, y2]);
    let d = `M${f(x1)} ${f(y1)}`;
    for (let i = 1; i < pts.length; i++) {
      const [xa, ya] = pts[i - 1], [xb, yb] = pts[i];
      if (i % 2 === 0) {  // down through a row's gap
        d += `L${f(xb)} ${f(yb)}`;
      } else {
        const dy = Math.max((yb - ya) / 2, pts.length === 2 ? 12 : 4);
        d += `C${f(xa)} ${f(ya + dy)} ${f(xb)} ${f(yb - dy)} ${f(xb)} ${f(yb)}`;
      }
    }
    const [ka, kb] = [a, b];
    wires.append(edgeWire(ka, kb, d, label, kinds));
    wires.append(svgEl("path", { "data-from": ka, "data-to": kb, class: "head",
                                 d: `M${f(x2 - HEAD_W)} ${f(y2)}L${f(x2)} ${f(end.y)}`
                                   + `L${f(x2 + HEAD_W)} ${f(y2)}z` }));
    // its names twice: by the far end from whichever card is traced, so the names of a card's
    // edges spread out over the cards around it instead of piling up on it. Plain order has
    // none: the arrow says it.
    if (!label) continue;
    const pair = JSON.stringify([ka, kb]);  // one path a pair (see merge), so one lane of names
    const lane = labelLanes.get(pair) || 0;
    labelLanes.set(pair, lane + 1);
    const near = [[pts[0], pts[1], "from"], [pts[pts.length - 2], pts[pts.length - 1], "to"]];
    for (const [[xa, ya], [xb, yb], end] of near) {
      const text = svgEl("text", { "data-from": ka, "data-to": kb, "data-end": end,
                                   x: f((xa + xb) / 2), y: f((ya + yb) / 2 + 4 + lane * 14) });
      text.textContent = label;
      names.append(text);
    }
  }
  svg.replaceChildren(wires, names);
}

// One line's path, named for what it carries (plain order: "after").
function edgeWire(a, b, d, label, kinds) {
  const attrs = { "data-from": a, "data-to": b, d };
  if (kinds.includes("tolerant")) attrs.class = "order";
  attrs["data-kind"] = kinds[0];
  const wire = svgEl("path", attrs), title = svgEl("title", {});
  title.textContent = label || "after";
  wire.append(title);
  return wire;
}

function trace(host, key) {
  const near = new Set([key]);
  for (const el of $$("[data-from]", host)) {
    const from = el.dataset.from === key, to = el.dataset.to === key;
    const end = el.dataset.end;  // a name shows by the other card
    const on = end ? (from && end === "to") || (to && end === "from") : from || to;
    el.classList.toggle("on", on);
    if (from || to) near.add(el.dataset.from).add(el.dataset.to);
  }
  // a card folded away in a done unit: its unit's line stands in for it, ringed
  for (const k of [...near]) {
    const card = k.startsWith("s:") && $(`.node[data-node="${CSS.escape(k)}"]`, host);
    const box = card && !shown(card) && card.closest(".box[data-node]");
    if (box) near.add(box.dataset.node);
  }
  for (const n of $$(".node, .box[data-node]", host)) {
    n.classList.toggle("near", near.has(n.dataset.node));
  }
  // a unit folded away on the closed shelf of done units: the shelf's line stands in for it
  const shelves = new Set();
  for (const k of near) {
    const box = k.startsWith("u:") && $(`.box[data-node="${CSS.escape(k)}"]`, host);
    const shelf = box && !shown(box) && box.closest("details.done-shelf");
    if (shelf) shelves.add(shelf);
  }
  for (const s of $$("details.done-shelf", host)) s.classList.toggle("near", shelves.has(s));
  const plane = $(".plane", host);
  if (plane && !plane.classList.contains("tracing")) plane.classList.add("tracing");
}

function untrace(host) {
  const plane = $(".plane", host);
  if (plane?.classList.contains("tracing")) plane.classList.remove("tracing");
  for (const el of $$(".on, .near", host)) el.classList.remove("on", "near");
}

// The card an arrow key goes to: left/right the nearest in the same row; down/up one in the
// next row that way (the nearest row, never one further), one the card is joined to by an edge
// when there is one, else the nearest across.
function nearestCard(here, evt, edges) {
  const dir = { ArrowDown: [0, 1], ArrowUp: [0, -1], ArrowRight: [1, 0],
                ArrowLeft: [-1, 0] }[evt.key];
  if (!dir) return null;
  const r = here.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2;
  const cands = [];
  for (const n of $$(".node[data-node]", here.closest(".plane"))) {
    if (n === here || !shown(n)) continue;
    const q = n.getBoundingClientRect(), nx = q.left + q.width / 2, ny = q.top + q.height / 2;
    const along = dir[0] ? (nx - x) * dir[0] : (ny - y) * dir[1];
    const across = dir[0] ? Math.abs(ny - y) : Math.abs(nx - x);
    if (along <= 4 || (dir[0] && across > r.height / 2)) continue;  // left/right: same row
    cands.push({ n, along, across });
  }
  if (!cands.length) return null;
  const by = (a, b) => a.across - b.across;
  if (dir[0]) return cands.sort((a, b) => a.along + a.across * 2 - b.along - b.across * 2)[0].n;
  const first = Math.min(...cands.map((c) => c.along));
  const row = cands.filter((c) => c.along < first + r.height / 2);
  const key = here.dataset.node;
  const joined = row.filter((c) => (Array.isArray(edges) ? edges : []).some(
    ([a, b]) => (a === key && b === c.n.dataset.node) || (b === key && a === c.n.dataset.node)));
  return (joined.length ? joined : row).sort(by)[0].n;
}

// A card's state: its one `is-<state>` class (the status table's key; `is-next` is no state).
const stateOf = (classes) => /\bis-(?!next\b)\w+/.exec(classes || "")?.[0];

// A status the live board moved on, said once to a screen reader: "a failed".
function announce(text) {
  const live = document.getElementById("announce");
  if (!live || !text) return;
  live.textContent = "";
  requestAnimationFrame(() => { live.textContent = text; });
}

// A folded finished box remembers, per tab, that it was opened.
const BOXES = "sluice.boxes";
function boardEdges(host, edges) {
  const key = (end) => ({step: "s", unit: "u", input: "i", output: "o"}[end.kind] + ":" + end.id);
  return edges.map((e) => [key(e.from), key(e.to), e.label,
    [e.kind, ...(e.tolerant ? ["tolerant"] : [])]]);
}

// A relation in words, shown by its line while a card is traced: "summary → spec" (a value
// passed), "even if skipped", "if ok", "if not ok". Plain order, a step's or a unit's, needs
// none: the arrow says "this, then that".
function words(label, kinds) {
  if (kinds[0] === "ordering" || kinds[0] === "unit") return kinds.includes("tolerant") ? "even if skipped" : "";
  if (kinds[0] === "condition" || kinds[0] === "negated_condition") return `if ${label}`;
  return label;
}
// One path per pair of cards, its names every relation between them: a handoff and a gate on
// the same pair are one line, its kind the strongest relation's. A line whose order another
// path already gives (its source reaches its dependent through two lines or more) is dropped,
// a value passed along it too: the board shows what comes after what, and the step's page
// lists its inputs and gates. A condition and an order a skip satisfies stay: each says more
// than the order.
function merge(list) {
  const pairs = new Map();
  for (const [a, b, label, kinds] of list) {
    const key = `${a}\n${b}`;
    if (!pairs.has(key)) pairs.set(key, { a, b, words: [], kinds: new Set(), tolerant: true });
    const p = pairs.get(key), said = words(label, kinds);
    if (said && !p.words.includes(said)) p.words.push(said);
    p.kinds.add(kinds[0]);
    if (!kinds.includes("tolerant")) p.tolerant = false;
  }
  const next = new Map();
  for (const p of pairs.values()) {
    if (!next.has(p.a)) next.set(p.a, []);
    next.get(p.a).push(p.b);
  }
  // `b` reached from `a` by a path of two edges or more
  const implied = (a, b) => {
    const seen = new Set(), todo = (next.get(a) || []).filter((n) => n !== b);
    while (todo.length) {
      const n = todo.pop();
      if (n === b) return true;
      if (seen.has(n)) continue;
      seen.add(n);
      todo.push(...(next.get(n) || []));
    }
    return false;
  };
  const out = [];
  for (const p of pairs.values()) {
    const plain = [...p.kinds].every((k) => k === "ordering" || k === "handoff" || k === "unit");
    if (plain && !p.tolerant && implied(p.a, p.b)) continue;
    const kind = ["handoff", "condition", "negated_condition", "ordering"].find((k) => p.kinds.has(k))
      ?? [...p.kinds][0];
    out.push([p.a, p.b, p.words.join(" · "), [kind, ...(p.tolerant ? ["tolerant"] : [])]]);
  }
  return out;
}

// The relations the server marks as lines: within a box, and between units the view shows that
// are not done (a done source is satisfied; one left out is said in words on its dependent).
const drawn = (edges) => boardEdges(null, (Array.isArray(edges) ? edges : []).filter((e) => e.line));

function openBoxes() {
  try { return JSON.parse(sessionStorage.getItem(BOXES) || "{}") || {}; } catch { return {}; }
}
function restoreBoxes(host) {
  const open = openBoxes();
  for (const d of $$("details[data-box]", host)) {
    if (open[`${location.pathname}:${d.dataset.box}`] && !d.open) d.open = true;

  }
}

// A lane matrix never cuts a column off. Its stylesheet folds it by its pane's width (the
// summary under the title from 1100px of pane, rows as lane strings from 720px, earlier for many
// stages), but a pill's width is its content's (a long timer, a run's earlier glyphs), so the
// board measures too: a table still wider than its wrap gives up its summary column
// (`data-fit="nosum"`), then becomes lane strings (`data-fit="lane"`). Each fit starts from
// the whole table, so a matrix that has room again gets its columns back.
function fitMatrices(host) {
  for (const m of $$(".matrix", host)) {
    const wrap = $(":scope > .mx-wrap", m), table = wrap && $(":scope > .mx", wrap);
    if (!table || !shown(m)) continue;
    const css = getComputedStyle(wrap);
    const room = () => wrap.clientWidth - parseFloat(css.paddingLeft) - parseFloat(css.paddingRight);
    const over = () => table.getBoundingClientRect().width > room() + 0.5;
    if (m.hasAttribute("data-fit")) m.removeAttribute("data-fit");
    let fit = null;
    for (const next of ["nosum", "lane"]) {
      if (!over()) break;
      fit = next;
      m.setAttribute("data-fit", fit);
    }
  }
}

// These classes change highlighting or animation, without changing card geometry.
const presentation = new Set(["tracing", "near", "open", "flip", "on"]);
const layoutClasses = (value) => (value || "").split(/\s+/)
  .filter((c) => c && !presentation.has(c)).sort().join(" ");

// The board's relations are the JSON of its `script.board-edges`: a region of its own, which a
// patch redraws only when the plan's shape or a line changes, not with every card's state.
function boardRelations(host) {
  const text = $(":scope > script.board-edges", host)?.textContent ?? "[]";
  if (host.relationsText !== text) {
    host.relationsText = text;
    try { host.relations = JSON.parse(text); } catch { host.relations = []; }
  }
  return host.relations;
}

rocket("sluice-board", {
  mode: "light",
  renderOnPropChange: false,
  manifest: {
    slots: [{ name: "relations", description: "script.board-edges: the plan's relations as JSON, a region of its own." },
            { name: "plane", description: ".plane: the bands, boxes, matrices and cards, then svg.edges, which this draws." }],
    events: [],
  },
  setup({ host, cleanup }) {
    let frame = 0, active = true;
    // tracing follows the keyboard's focus, not a focus given back after a click or by the
    // drawer's close: `kept` is the card or name it traced, the one a pointer leaving (or a
    // redraw) goes back to. Any other focus traces nothing, so a trace never stays on with
    // nothing held.
    let kept = null;
    const focusKept = () => (kept === document.activeElement && host.contains(kept) ? kept : null);
    const listeners = new AbortController();
    // while the board's splitter is dragged the plan's edges hide and wait: one redraw at the
    // end ("sluice-resized"), not one per frame
    // the matrices fit on every frame of a drag too: a column is never cut, even for a moment
    const redraw = () => {
      if (!active || frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        fitMatrices(host);
        if (document.documentElement.classList.contains("resizing")) return;
        drawEdges(host, drawn(boardRelations(host)));
        const held = still ? null : $(`:is(${TRACES}):hover`, host) || focusKept();  // keep it lit
        if (held) trace(host, traceKey(held));
      });
    };
    const sizes = new ResizeObserver(redraw);
    sizes.observe(host);
    document.fonts?.ready.then(redraw);
    // a patch of the board: redraw, keep the open card marked, flip a glyph whose status moved
    const changes = new MutationObserver((records) => {
      let board = false;
      const said = new Set();
      for (const r of records) {
        if (r.target.closest?.("svg.edges")) continue;
        // a card's timer ticking (nav.js says when that widens a card)
        const at = r.target.nodeType === Node.TEXT_NODE ? r.target.parentElement : r.target;
        if (r.type !== "attributes" && at?.closest?.("time[data-since]")) continue;
        if (r.type === "attributes" && r.attributeName === "class"
            && layoutClasses(r.oldValue) === layoutClasses(r.target.getAttribute("class"))) continue;
        board = true;
        const el = r.target;
        if (r.type === "attributes" && el.classList?.contains("node")
            && stateOf(el.className) !== stateOf(r.oldValue)) {
          const g = $(".g", el);
          g?.classList.remove("flip");
          void g?.offsetWidth;
          g?.classList.add("flip");
          // the glyph's name is the state's word ("quiet", "stopping"): said once, politely
          const word = $(".g[aria-label]", el)?.getAttribute("aria-label");
          if (word) said.add(`${el.dataset.node.slice(2)} ${word}`);
        }
      }
      if (!board) return;
      announce([...said].join(". "));
      restoreBoxes(host);
      const sid = currentStep();
      if (sid && !document.getElementById(`n-${sid}`)?.classList.contains("open")) markOpen(sid);
      redraw();
    });
    changes.observe(host, { childList: true, subtree: true, characterData: true,
                            attributes: true, attributeFilter: ["class", "data-box-version"],
                            attributeOldValue: true });
    const card = (evt) => evt.target.closest?.(".node[data-node]");
    const tracer = (evt) => evt.target.closest?.(TRACES);
    const over = (evt) => { const n = tracer(evt); if (n && !still) trace(host, traceKey(n)); };
    const out = (evt) => {
      const n = tracer(evt);
      if (!n || n.contains(evt.relatedTarget)) return;
      const back = focusKept();
      if (back) trace(host, traceKey(back)); else untrace(host);
    };
    const focus = (evt) => {
      const n = tracer(evt);
      kept = n && !still && n.matches(":focus-visible") ? n : null;
      if (kept) trace(host, traceKey(kept)); else untrace(host);
    };
    const keys = (evt) => {
      still = false;
      const here = card(evt);
      if (!here || evt.altKey || evt.ctrlKey || evt.metaKey) return;
      const next = nearestCard(here, evt, boardEdges(host, boardRelations(host)));
      if (next) { evt.preventDefault(); next.focus(); }
    };
    host.addEventListener("pointerover", over, { signal: listeners.signal });
    host.addEventListener("pointerout", out, { signal: listeners.signal });
    host.addEventListener("focusin", focus, { signal: listeners.signal });
    host.addEventListener("focusout", (evt) => {
      if (host.contains(evt.relatedTarget)) return;
      kept = null;
      untrace(host);
    }, { signal: listeners.signal });
    host.addEventListener("keydown", keys, { signal: listeners.signal });
    host.addEventListener("toggle", (evt) => {
      const d = evt.target;
      if (!d.matches?.("details[data-box]")) return;
      redraw();
      if (d.hasAttribute("data-forced")) return;  // opened for a search or a filter, not by hand
      const open = openBoxes(), k = `${location.pathname}:${d.dataset.box}`;
      if (d.open) open[k] = 1; else delete open[k];
      try { sessionStorage.setItem(BOXES, JSON.stringify(open)); } catch { /* no storage */ }
    }, { capture: true, signal: listeners.signal });
    PHONE.addEventListener("change", redraw);
    window.addEventListener("sluice-resized", redraw, { signal: listeners.signal });
    restoreBoxes(host);
    markOpen(currentStep());
    cleanup(() => {
      active = false;
      listeners.abort();
      cancelAnimationFrame(frame);
      sizes.disconnect();
      changes.disconnect();
      PHONE.removeEventListener("change", redraw);
    });
  },
  onFirstRender({ host }) {
    fitMatrices(host);
    drawEdges(host, drawn(boardRelations(host)));
  },
});

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
      if (sid || last) still = true;  // it opens, moves on or closes: the page reflows
      document.documentElement.classList.toggle("drawer-open", Boolean(sid));
      drawer.hidden = !sid;
      $(".scrim", host).hidden = !sid;
      modal();
      markOpen(sid);
      for (const b of $$("sluice-board")) untrace(b);  // the drawer shows the step, undimmed
      if (!sid) {
        stream?.abort();
        $("#drawer-stream").replaceChildren();
        mergePatch({ step: "", sver: "" });
        tabInAddress("");
        // back to the card (beside the drawer, the page brought it into view); a phone's sheet
        // did not, so a name in a card's waits that opened it takes the focus back
        const card = last && document.getElementById(`n-${last}`);
        const named = PHONE.matches && opener?.matches(".waits a") ? opener : null;
        (named || card || opener)?.focus({ preventScroll: true });
        for (const board of $$("sluice-board")) untrace(board);
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
      // a matrix too narrow for its columns draws each row's stages as links in its lane line
      const ids = [...new Set($$("sluice-board a[data-step], sluice-board .mx-lane a[data-opens]")
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
      + ".node, .scrim, [data-step]";
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
