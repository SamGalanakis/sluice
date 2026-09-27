"""Views (SPEC §8): the HTML of the dashboard `sluice serve` renders, and `plan_view`.

- `mermaid(plan, state)`: the plan as a Mermaid flowchart (plan_view's text format, for agents).
- `index`: every project as one row, under the compact "Needs you" lines of every project.
- `project_page`: the project's "Needs you" lines, then its plan as a board of cards laid out by
  dependency depth (plan inputs left, plan outputs right, edges as inline SVG), drawn entirely
  on the server; a card opens the step's detail (`step_detail`): a drawer on the live page, a
  page of its own without JavaScript. `render()` serves the standalone page to `plan_view`.
- `fns_page`: every visible function grouped by scope; `log_page`: one page of a log, filtered
  (`LogQuery`); `inbox_page`: the items waiting on a person, each open one with its answer box
  (drawn from its OpenUI program by `static/inbox.js`).

A live page (given its stream URL) loads Datastar and opens one SSE stream; `dashboard` sends
the parts that changed, re-rendered by the same `*_parts` functions, as element patches. Each
part is one element with an id. Everything here only reads the store (the inbox's answer route
lives in `dashboard`), and every value is HTML-escaped (plans, logs, run output and inbox items
are untrusted; markdown bodies are rendered with raw HTML disabled).
"""

from __future__ import annotations

import dataclasses
import datetime as dt
import html
import json
import re
from collections.abc import Iterable, Mapping
from pathlib import Path
from typing import Any
from urllib.parse import quote, urlencode

from markdown_it import MarkdownIt

from . import log as L
from . import types as T
from .errors import BadRequest, NotFound, SluiceError
from .plan import Plan, parse_ref, value_of
from .store import Store
from .util import read_json, tail_text

CLASSES = {"pending": "fill:#f1f1f1,stroke:#999,color:#333",
           "running": "fill:#dbeafe,stroke:#2563eb,color:#1e3a8a",
           "succeeded": "fill:#dcfce7,stroke:#16a34a,color:#14532d",
           "failed": "fill:#fee2e2,stroke:#dc2626,color:#7f1d1d",
           "stale": "fill:#fef3c7,stroke:#d97706,color:#78350f",
           "manual": "fill:#fff,stroke:#16a34a,stroke-width:3px,stroke-dasharray:6 3"}
STATUSES = ("pending", "running", "succeeded", "stale", "failed")
DATASTAR_JS = "https://cdn.jsdelivr.net/gh/starfederation/datastar@v1.0.4/bundles/datastar.js"
FONT_CSS = "https://cdn.jsdelivr.net/npm/@fontsource-variable/inter@5.3.0/index.css"
# Keep the stream open across server restarts and network blips (Datastar backs off to 30 s).
# Reconnect for good, and within 3 s once the server is back (Datastar backs off to 30 s).
STREAM_OPTIONS = "{retry: 'always', retryMaxCount: 1000000, retryMaxWait: 3000}"
PAGE_SIZE = 50  # log records per log page
SCOPE_TITLES = {"builtin": "Built-in", "global": "Global", "project": "Project"}
# The log viewer's kind filter: each group name, then the kinds under it (§6b).
KIND_OPTIONS = tuple(dict.fromkeys(
    x for k in L.KINDS for x in [*(g for g in L.GROUPS if k.startswith(g + ".")), k]))
HISTORY_QUERY = urlencode([("kind", k) for k in L.HISTORY_KINDS])
PROMPT_INPUTS = ("prompt", "spec", "task", "instructions", "brief")  # an agent block's prompt
TEXT_OUTPUTS = ("result", "summary", "text", "message", "answer")  # what a card shows first
TAIL = 6000  # characters of a run's stderr in the step detail

STATIC = Path(__file__).resolve().parent / "static"
CSS = (STATIC / "dashboard.css").read_text(encoding="utf-8")

e = html.escape
# CommonMark plus tables; raw HTML is escaped as text and unsafe link schemes are refused.
MARKDOWN = MarkdownIt("commonmark", {"html": False}).enable("table")
INBOX_FILTERS = ("open", "answered", "closed", "all")


def markdown(text: str) -> str:
    return MARKDOWN.render(text)


# ---- Mermaid (plan_view's text format) ----------------------------------------------------


def _q(text: str) -> str:
    return '"' + text.replace('"', "#quot;") + '"'


def _mermaid_text(text: str, width: int = 60) -> str:
    """Free text (a step's doc) safe inside a quoted Mermaid label: one line, no markup."""
    line = " ".join(text.split())
    line = line if len(line) <= width else line[:width - 1] + "…"
    return line.replace("#", "#35;").replace("<", "#lt;").replace(">", "#gt;")


def step_label(sid: str, run: str, entry: dict[str, Any], doc: str = "") -> str:
    """`id / fn / status`, then the step's doc on a second line."""
    label = f"{sid} / {run} / {_status(entry)}"
    return label + (f"<br/>{_mermaid_text(doc)}" if doc else "")


def _status(entry: dict[str, Any]) -> str:
    status = entry["status"]
    if "total" in entry:
        status += f" {entry.get('done', 0)}/{entry['total']}"
    return status


def mermaid(plan: Plan, state: dict[str, Any]) -> str:
    ids = {("in", n): f"in{i}" for i, n in enumerate(plan.inputs)}
    ids.update({("step", s): f"s{i}" for i, s in enumerate(plan.steps)})
    ids.update({("out", n): f"out{i}" for i, n in enumerate(plan.outputs)})
    lines = ["flowchart LR"]
    for n in plan.inputs:
        lines.append(f"  {ids['in', n]}([{_q(n)}])")
    for sid, step in plan.steps.items():
        entry = state["steps"].get(sid, {"status": "pending"})
        label = step_label(sid, step.fn.name, entry, step.doc)
        lines.append(f"  {ids['step', sid]}[{_q(label)}]")
    for n in plan.outputs:
        lines.append(f"  {ids['out', n]}([{_q(n)}])")

    def edge(ref, target: str) -> str:
        src = ids["step", ref.step] if ref.step else ids["in", ref.name]
        return f"  {src} -->|{_q(ref.name)}| {target}"

    for sid, step in plan.steps.items():
        lines.extend(dict.fromkeys(edge(r, ids["step", sid]) for r in step.reads))
        lines.extend(f"  {ids['step', a]} -.->|after| {ids['step', sid]}" for a in step.after)
    for n, ref in plan.outputs.items():
        lines.append(edge(ref, ids["out", n]))
    for cls, style in CLASSES.items():
        lines.append(f"  classDef {cls} {style}")
    for sid in plan.steps:
        entry = state["steps"].get(sid, {"status": "pending"})
        cls = "manual" if entry.get("manual") and entry["status"] == "succeeded" \
            else entry["status"]
        lines.append(f"  class {ids['step', sid]} {cls}")
    return "\n".join(lines) + "\n"


# ---- small formatters -------------------------------------------------------------------


def _json(value: Any) -> str:
    return e(json.dumps(value, indent=2, ensure_ascii=False))


def _line(text: Any, width: int = 120) -> str:
    """The first line of `text`, at most `width` characters, with … when anything is cut."""
    lines = str(text).strip().splitlines() or [""]
    line, more = lines[0], len(lines) > 1
    if len(line) > width:
        line, more = line[:width - 1], True
    return line + ("…" if more else "")


def _parse_iso(iso: Any) -> dt.datetime | None:
    try:
        return dt.datetime.strptime(str(iso), "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.UTC)
    except ValueError:
        return None


def _now() -> dt.datetime:
    return dt.datetime.now(dt.UTC)


def dur(seconds: float) -> str:
    """A duration on one ladder: `0.4s`, `7.2s`, `42s`, `12m 4s`, `1h 5m` (zero parts dropped;
    seconds only under an hour)."""
    if seconds < 10:
        return f"{max(seconds, 0):.1f}s".replace(".0s", "s")
    s = int(seconds)
    if s < 60:
        return f"{s}s"
    d, rem = divmod(s, 86400)
    h, rem = divmod(rem, 3600)
    m, sec = divmod(rem, 60)
    parts = [(d, "d"), (h, "h"), (m, "m")] + ([(sec, "s")] if not d and not h else [])
    return " ".join(f"{n}{u}" for n, u in parts if n) or "0s"


def _age(iso: str, now: dt.datetime | None = None) -> str:
    """`5m ago` from an ISO timestamp (the age when the page was drawn)."""
    then = _parse_iso(iso)
    if then is None:
        return str(iso)
    secs = int(((now or _now()) - then).total_seconds())
    for unit, size in (("d", 86400), ("h", 3600), ("m", 60)):
        if secs >= size:
            return f"{secs // size}{unit} ago"
    return "just now"


def _when(iso: str) -> str:
    """A relative time the page keeps current (`data-ago`, static/board.js)."""
    return f'<time datetime="{e(iso)}" title="{e(iso)}" data-ago>{e(_age(iso))}</time>'


def _elapsed(block: Block) -> str:
    """How long a step ran (live for a running one: `data-since`), or ''."""
    start, end = _parse_iso(block.entry.get("started")), _parse_iso(block.entry.get("finished"))
    if start is None or block.status == "pending":
        return ""
    if block.status == "running":
        iso = e(block.entry["started"])
        return (f'<time datetime="{iso}" data-since="{iso}">'
                f"{e(dur((_now() - start).total_seconds()))}</time>")
    return e(dur((end - start).total_seconds())) if end else ""


def _money(cost: float | None) -> str:
    return "" if cost is None else f"${cost:,.2f}"


def _signals(values: Mapping[str, Any]) -> str:
    return e(json.dumps(values, ensure_ascii=False))


# ---- glyphs -----------------------------------------------------------------------------
# One outcome glyph per status (shape carries it, colour repeats it); drawn, not typed.

_RING = '<circle cx="8" cy="8" r="5.5" fill="none" stroke="currentColor" stroke-width="1.5"'
_DISC = '<circle cx="8" cy="8" r="6.5" fill="currentColor"/>'
_CUT = 'fill="none" stroke="var(--card)" stroke-width="1.6" stroke-linecap="round"'
GLYPHS = {
    "pending": _RING + ' stroke-dasharray="2.6 2.2"/>',
    "running": _RING + ' opacity=".25"/><path class="spin" d="M8 2.5a5.5 5.5 0 0 1 5.5 5.5" '
               'fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>',
    "succeeded": _DISC + f'<path d="M5.2 8.3l1.9 1.9 3.7-4.1" {_CUT} stroke-linejoin="round"/>',
    "manual": _RING + '/><circle cx="8" cy="8" r="2.6" fill="currentColor"/>',
    "stale": '<path d="M12.6 5.6A5.1 5.1 0 1 0 13.1 8.6" fill="none" stroke="currentColor" '
             'stroke-width="1.5" stroke-linecap="round"/><path d="M13.2 2.7v3.4H9.8" '
             'fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" '
             'stroke-linejoin="round"/>',
    "failed": _DISC + f'<path d="M5.9 5.9l4.2 4.2M10.1 5.9l-4.2 4.2" {_CUT}/>',
    "paused": _RING + '/><path d="M6.6 5.9v4.2M9.4 5.9v4.2" fill="none" stroke="currentColor" '
              'stroke-width="1.5" stroke-linecap="round"/>',
}
WORDS = {"manual": "set by hand", "paused": "paused"}
X_ICON = ('<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path '
          'd="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.5" '
          'stroke-linecap="round"/></svg>')


def glyph(status: str) -> str:
    """The status glyph with its word for assistive technology."""
    word = WORDS.get(status, status)
    return (f'<span class="g g-{e(status)}" title="{e(word)}"><svg viewBox="0 0 16 16" '
            f'width="16" height="16" aria-hidden="true">{GLYPHS.get(status, GLYPHS["pending"])}'
            f'</svg><span class="vh">{e(word)}</span></span>')


# ---- layout -----------------------------------------------------------------------------


def open_count(store: Store) -> int:
    """How many inbox items wait on a person, across every project."""
    return len(store.inbox())


# the Inbox's icon, shown in place of its word on a phone
TRAY = ('<svg class="tray" viewBox="0 0 20 20" width="20" height="20" aria-hidden="true">'
        '<path d="M3 11.5 5 4.5h10l2 7M3 11.5V15.5h14v-4M3 11.5h4l1 2h4l1-2h4" fill="none" '
        'stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>')


def nav_inbox(count: int | None, current: bool = False) -> str:
    """The nav's Inbox link: its count of open items is the dashboard's one red badge."""
    badge = f' <span class="badge" title="open items">{count}</span>' if count else ""
    cur = ' aria-current="page"' if current else ""
    return f'<a id="nav-inbox" href="/inbox"{cur}>{TRAY}<span class="t">Inbox</span>{badge}</a>'


NAV = (("/", "Projects"), ("/log", "Log"), ("/fns", "Functions"))  # with no project chosen
# The wordmark's mark: a gate across a channel, in ink.
BRAND_MARK = ('<svg class="mark" viewBox="0 0 20 20" width="20" height="20" aria-hidden="true">'
              '<rect width="20" height="20" rx="5" fill="currentColor"/>'
              '<path d="M4 13.5c2-1.6 4-1.6 6 0s4 1.6 6 0" fill="none" stroke="var(--card)" '
              'stroke-width="1.6" stroke-linecap="round"/>'
              '<path d="M7 4.5v5.5M13 4.5v5.5M7 7h6" fill="none" stroke="var(--card)" '
              'stroke-width="1.6" stroke-linecap="round"/></svg>')
CHEVRON = ('<svg class="chev" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">'
           '<path d="M4.5 6.5 8 10l3.5-3.5" fill="none" stroke="currentColor" stroke-width="1.6" '
           'stroke-linecap="round" stroke-linejoin="round"/></svg>')
PROJECT_TABS = ("plan", "log", "history", "fns")  # a project's sections, in the nav


def project_head(project: str, tab: str | None) -> str:
    """A project page's title: the nav already names the project (its switcher) and the
    section, so it is for assistive technology only; visible on the standalone page
    (`tab` None), which has no nav."""
    if tab is None:
        return f'<div class="phead"><h1>{e(project)}</h1></div>'
    return f'<h1 class="vh">{e(project)}</h1>'


def _project_mark(counts: Mapping[str, int]) -> str:
    """One glyph for a whole project: running, failed, stale, finished, or waiting."""
    total = sum(counts.values())
    for status in ("running", "failed", "stale"):
        if counts.get(status):
            return glyph(status)
    return glyph("succeeded" if total and counts.get("succeeded") == total else "pending")


def _current(item: str, here: str, sub: bool = False) -> str:
    """The aria-current of a nav item: `page` for the page itself, `true` for a page inside it
    (`sub`), else nothing."""
    if item != here:
        return ""
    return ' aria-current="true"' if sub else ' aria-current="page"'


def project_switcher(store: Store, project: str | None) -> str:
    """The nav's project switcher: its button is the project's name ("All projects" when none
    is chosen); the menu lists every project, the archived ones last. A <details>, so it works
    without JavaScript."""
    items, old = [], []
    for info in store.projects():
        name = info["name"]
        cur = ' aria-current="page"' if name == project else ""
        row = (f'<a href="/projects/{e(quote(name))}"{cur}>{_project_mark(info["counts"])}'
               f"<span>{e(name)}</span></a>")
        (old if info.get("archived") else items).append(row)
    cur = ' aria-current="page"' if project is None else ""
    menu = f'<a href="/" class="all"{cur}>All projects</a>' + "".join(items)
    if old:
        menu += f'<p class="menu-label">Archived</p>{"".join(old)}'
    label = e(project) if project else "All projects"
    return (f'<details class="switcher"><summary aria-label="Project: {label}">'
            f'<span class="sw-name">{label}</span>{CHEVRON}</summary>'
            f'<div class="menu">{menu}</div></details>')


def top_nav(store: Store | None, project: str | None, tab: str | None, here: str,
            inbox: int | None, sub: bool = False) -> str:
    """The one nav: the mark, the project switcher, the sections of the chosen project (Plan,
    Log, History, Functions) or of all of them (Projects, Log, Functions), and the Inbox with
    the one red badge."""
    if project is not None:
        p = quote(project)
        hrefs = {"plan": (f"/projects/{p}", "Plan"), "log": (f"/projects/{p}/log", "Log"),
                 "history": (f"/projects/{p}/log?{HISTORY_QUERY}", "History"),
                 "fns": (f"/fns?project={p}", "Functions")}
        links = "".join(f'<a href="{e(href)}"{_current(t, tab or "", sub)}>{text}</a>'
                        for t, (href, text) in ((t, hrefs[t]) for t in PROJECT_TABS))
    else:
        links = "".join(f'<a href="{href}"{_current(href, here)}>{text}</a>'
                        for href, text in NAV)
    switcher = project_switcher(store, project) if store is not None else ""
    return (f'<nav class="top" aria-label="Sections"><a class="brand" href="/" '
            f'aria-label="sluice: all projects">{BRAND_MARK}</a>{switcher}'
            f'<span class="links">{links}</span>{nav_inbox(inbox, here == "/inbox")}</nav>')


def layout(title: str, body: str, nav: bool = True, stream: str | None = None,
           signals: Mapping[str, Any] | None = None, main_attrs: str = "",
           inbox: int | None = None, script: str = "", here: str = "", sub: bool = False,
           board: bool = False, store: Store | None = None, project: str | None = None,
           tab: str | None = None) -> str:
    """A page. With `stream`, Datastar opens that SSE stream once the page has loaded (with
    `signals`, the page's Datastar signals, sent along as the `datastar` query parameter).
    `inbox` is the count of open items for the nav's badge; `script` a module to load; `here`
    the nav entry of this page, `project` and `tab` the chosen project and its section (`sub`:
    a page inside that entry, as a step is inside Plan); `store` lists the projects for the
    nav's switcher; `board` loads the board's script (times, drawer, tracing)."""
    head = f'<script type="module" src="{DATASTAR_JS}"></script>' if stream else ""
    scripts = "".join(f'<script type="module" src="{e(s)}"></script>'
                      for s in (script, "/static/board.js" if board else "",
                                "/static/nav.js" if nav else "") if s)
    top = top_nav(store, project, tab, here, inbox, sub) if nav else ""
    body_attrs = f' data-signals="{_signals(signals)}"' if signals else ""
    if stream:
        main_attrs += f' data-init="@get(\'{e(stream)}\', {STREAM_OPTIONS})"'
    return (f'<!doctype html>\n<html lang="en"><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width,initial-scale=1">'
            f"<title>{e(title)} · sluice</title>"
            f'<link rel="stylesheet" href="{FONT_CSS}"><style>{CSS}</style>{head}</head>\n'
            f"<body{body_attrs}>{top}<main{main_attrs}>\n{body}\n</main>{scripts}</body></html>\n")


def _part(pid: str, inner: str, tag: str = "div", cls: str = "") -> str:
    c = f' class="{cls}"' if cls else ""
    return f'<{tag} id="{pid}"{c}>{inner}</{tag}>'


def not_found(message: str) -> str:
    return layout("not found", f'<p class="lead">{e(message)}</p>')


def _label(text: str) -> str:
    return f'<h3 class="label">{text}</h3>'


# ---- a project's blocks -----------------------------------------------------------------


@dataclasses.dataclass
class Block:
    """One step as the dashboard shows it, read from plan.json (raw, so bindings and declared
    outputs it does not know yet still show), the parsed plan and state.json."""

    sid: str
    fn: str
    doc: str
    raw: dict[str, Any]
    entry: dict[str, Any]
    glue: bool  # a built-in that runs inline (core.*): a slim chip on the board
    fn_inputs: dict[str, str]  # its fn's inputs, then any extra ones it binds (open fns)
    fn_outputs: dict[str, str]  # its declared outputs first (open fns), then its fn's
    output_docs: dict[str, str] = dataclasses.field(default_factory=dict)

    @property
    def status(self) -> str:
        return self.entry.get("status", "pending")

    @property
    def mark(self) -> str:
        """The glyph's status: `manual` for a value set by hand, `paused` for a held step that
        has not started."""
        if self.paused and self.status == "pending":
            return "paused"
        return "manual" if self.entry.get("manual") and self.status == "succeeded" \
            else self.status

    @property
    def paused(self) -> bool:
        p = self.raw.get("paused")
        return p is True or isinstance(p, str) and bool(p.strip())

    @property
    def pause_reason(self) -> str:
        p = self.raw.get("paused")
        return p.strip() if isinstance(p, str) else ""

    @property
    def after(self) -> list[str]:
        a = self.raw.get("after")
        return [x for x in a if isinstance(x, str)] if isinstance(a, list) else []

    @property
    def tags(self) -> list[str]:
        t = self.raw.get("tags")
        return [x for x in t if isinstance(x, str)] if isinstance(t, list) else []

    @property
    def title(self) -> str:
        return " ".join(self.doc.split()) or self.sid

    @property
    def bindings(self) -> dict[str, Any]:
        b = self.raw.get("in")
        return b if isinstance(b, dict) else {}

    def refs(self, name: str) -> list[Any]:
        """The parsed refs one binding reads (none for a default)."""
        src = self.bindings.get(name)
        if not isinstance(src, dict) or "source" not in src:
            return []
        texts = src["source"] if isinstance(src["source"], list) else [src["source"]]
        return [r for r in (parse_ref(t)[0] for t in texts) if r is not None]

    @property
    def deps(self) -> list[str]:
        """The steps it waits for: those it reads from, then those it runs after."""
        return list(dict.fromkeys([*(r.step for n in self.bindings for r in self.refs(n)
                                     if r.step), *self.after]))

    @property
    def outputs(self) -> dict[str, str]:
        """Output name → type: what the step declares, then what its fn returns."""
        return dict(self.fn_outputs)

    @property
    def cost(self) -> float | None:
        v = (self.entry.get("outputs") or {}).get("cost_usd") \
            if isinstance(self.entry.get("outputs"), dict) else None
        return float(v) if isinstance(v, int | float) and not isinstance(v, bool) else None

    @property
    def agent(self) -> bool:
        return self.fn.startswith("agent.") or any(n in self.bindings for n in PROMPT_INPUTS)

    def default(self, name: str) -> Any:
        src = self.bindings.get(name)
        return src.get("default") if isinstance(src, dict) else None

    @property
    def engine(self) -> str:
        """What runs it: `claude · sonnet` for an agent block (from the fn name and a bound
        `engine`/`model`), else the fn's name."""
        if not self.fn.startswith("agent."):
            return self.fn
        engine = self.default("engine")
        parts = [engine if isinstance(engine, str) and engine else self.fn.split(".", 1)[1]]
        model = self.default("model")
        if isinstance(model, str) and model:
            parts.append(model)
        return " · ".join(parts)

    @property
    def run_ids(self) -> list[str]:
        return [r for r in self.entry.get("run_ids") or [] if isinstance(r, str)]


@dataclasses.dataclass
class Board:
    project: str
    info: dict[str, Any]
    doc: dict[str, Any]
    plan: Plan
    state: dict[str, Any]
    blocks: dict[str, Block]

    @property
    def counts(self) -> dict[str, int]:
        out: dict[str, int] = {}
        for b in self.blocks.values():
            out[b.status] = out.get(b.status, 0) + 1
        return out

    @property
    def cost(self) -> float | None:
        costs = [b.cost for b in self.blocks.values() if b.cost is not None]
        return sum(costs) if costs else None


def load_board(store: Store, project: str) -> Board:
    info = store.project(project)
    doc, plan = store.plan(project)
    state = store.read_state(project)
    raw_steps = doc.get("steps") if isinstance(doc.get("steps"), dict) else {}
    blocks = {}
    for sid, step in plan.steps.items():
        raw = raw_steps.get(sid) if isinstance(raw_steps.get(sid), dict) else {}
        declared = step.declared
        extra = step.extra
        blocks[sid] = Block(
            sid, step.fn.name, step.doc or "", raw,
            state["steps"].get(sid, {"status": "pending"}), step.fn.native,
            {k: str(v) for k, v in {**step.fn.inputs, **extra}.items()},
            {k: str(v) for k, v in {**declared, **step.fn.outputs}.items()},
            dict(step.output_docs))
    return Board(project, info, doc, plan, state, blocks)


def _run_dir(store: Store, project: str, run_id: str) -> Path | None:
    """A run's directory, refusing anything that is not a plain run id."""
    if not L.RUN_ID_RE.match(run_id):
        return None
    return store.runs_dir(project) / run_id


def progress_line(store: Store, project: str, block: Block) -> str:
    """The last non-empty line its current run wrote to stderr (a running step's progress)."""
    if not block.run_ids:
        return ""
    d = _run_dir(store, project, block.run_ids[-1])
    text = tail_text(d / "stderr.log", 4000) if d else ""
    for line in reversed(text.splitlines()):
        if line.strip():
            return _line(line.strip(), 240)
    return ""


def _short(value: Any, width: int = 120) -> str:
    """A value in one line: text as it is, anything else as compact JSON."""
    if isinstance(value, str):
        return _line(value, width)
    return _line(json.dumps(value, ensure_ascii=False), width)


def output_summary(block: Block) -> str:
    """What a finished step produced, in one line: its first text output (result, summary…),
    else `name: value` of its first output."""
    outs = block.entry.get("outputs")
    if not isinstance(outs, dict):
        return ""
    names = [n for n in (*TEXT_OUTPUTS, *block.outputs, *outs)
             if n in outs and n not in ("cost_usd", "session")]
    for n in dict.fromkeys(names):
        v = outs[n]
        if isinstance(v, str) and v.strip():
            return _line(v, 200)
    for n in dict.fromkeys(names):
        if outs[n] is not None:
            return f"{n}: {_short(outs[n], 80)}"
    return ""


def _missing_inputs(board: Board, block: Block) -> list[str]:
    return list(dict.fromkeys(
        r.name for n in block.bindings for r in block.refs(n)
        if r.step is None and not value_of(r, board.plan, board.state)[0]))


def block_line(store: Store, board: Board, block: Block) -> tuple[str, str]:
    """(kind, text) of the card's one line: progress while running, the error when failed,
    what it produced when done, why it waits when a plan input holds it up."""
    status = block.status
    if status == "running":
        return "progress", progress_line(store, board.project, block)
    if status == "failed":
        return "error", _line(block.entry.get("error") or "failed", 200)
    if status == "stale":
        return "note", "Its inputs changed since it ran"
    if status == "succeeded":
        return "output", output_summary(block)
    missing = _missing_inputs(board, block)
    if missing:
        return "note", "Waits for " + ", ".join(missing)
    return "", ""


# ---- what needs a person ----------------------------------------------------------------


def _unanswered(store: Store, project: str, steps: Iterable[str]) -> list[dict[str, Any]]:
    """Messages addressed to the orchestrator or a person (anyone but a step of the plan) with
    no later message from that addressee on the same thread."""
    msgs = L.read(store.log_dir(project), kinds=["message"])["records"]
    steps = set(steps)
    out = []
    for i, m in enumerate(msgs):
        to = m.get("to")
        if not to or to in steps:
            continue
        if not any(x.get("thread") == m.get("thread") and x.get("from") == to
                   for x in msgs[i + 1:]):
            out.append(m)
    return out


def step_href(project: str, sid: str) -> str:
    return f"/projects/{quote(project)}/steps/{quote(sid)}"


def _thread_step(thread: Any, steps: Mapping[str, Any]) -> str | None:
    t = str(thread or "")
    return t[5:] if t.startswith("step-") and t[5:] in steps else None


def needs(store: Store, project: str) -> list[dict[str, str]]:
    """What waits on a person in one project, most actionable first: open inbox items, plan
    inputs that hold up a step, failed steps, unanswered messages. Each {kind, text (HTML),
    href, step?}."""
    out: list[dict[str, str]] = []
    base = f"/projects/{quote(project)}/inbox"
    for item in store.inbox(project):
        out.append({"kind": "Answer", "href": f"{base}#item-{e(project)}-{e(item['id'])}",
                    "text": f"{e(item['title'])} <span class=\"when\">asked "
                            f"{_when(item['created'])}</span>"})
    for w in store.waiting_inputs(project):
        doc = f" — {e(_line(w['doc'], 120))}" if w.get("doc") else ""
        out.append({"kind": "Input", "href": base,
                    "text": f"<b>{e(w['name'])}</b> has no value{doc}"})
    try:
        board = load_board(store, project)
    except SluiceError:  # e.g. a plan that no longer validates: its fix is the orchestrator's
        return out
    for b in board.blocks.values():
        if b.status == "failed":
            when = f' <span class="when">{_when(b.entry["finished"])}</span>' \
                if b.entry.get("finished") else ""
            out.append({"kind": "Failed", "href": step_href(project, b.sid), "step": b.sid,
                        "text": f"{e(b.title)}: <span class=\"err\">"
                                f"{e(_line(b.entry.get('error') or 'failed', 140))}</span>{when}"})
    for m in _unanswered(store, project, board.blocks):
        sid = _thread_step(m.get("thread"), board.blocks)
        href = step_href(project, sid) if sid else \
            f"/projects/{quote(project)}/log?thread={quote(str(m.get('thread') or ''))}"
        item = {"kind": "Message", "href": href,
                "text": f"{e(str(m.get('from') or '?'))} → {e(str(m.get('to')))}: "
                        f"{e(_line(m.get('body', ''), 140))} "
                        f"<span class=\"when\">{_when(m.get('at', ''))}</span>"}
        if sid:
            item["step"] = sid
        out.append(item)
    return out


def needs_band(items: list[dict[str, str]], link_steps: bool = True) -> str:
    """The "Needs you" lines (none when nothing waits)."""
    if not items:
        return ""
    rows = "".join(
        f'<li><a href="{it["href"]}"'
        + (f' data-step="{e(it["step"])}"' if link_steps and it.get("step") else "")
        + f'><span class="k k-{it["kind"].lower()}">{it["kind"]}</span>'
          f'<span class="t">{it["text"]}</span></a></li>' for it in items)
    return (f'<section class="needs" aria-labelledby="needs-h">'
            f'<h2 class="label attn" id="needs-h">Needs you ({len(items)})</h2>'
            f"<ul>{rows}</ul></section>")


# ---- the board: rows by dependency depth, inside the column -----------------------------

ROW_MAX = 4  # cards side by side in one row; more wrap onto another line of the same row
RUN_FACTS = ("session", "cost_usd")  # what a run says about itself, not what it produced


def depths(board: Board) -> dict[str, int]:
    """Each step's row: 0 for a step that reads no other step, else one below its deepest
    upstream."""
    depth: dict[str, int] = {}

    def row_of(sid: str) -> int:
        if sid not in depth:
            depth[sid] = 0  # (the plan is acyclic; this only guards the recursion)
            depth[sid] = max((row_of(d) + 1 for d in board.blocks[sid].deps
                              if d in board.blocks), default=0)
        return depth[sid]

    for sid in board.blocks:
        row_of(sid)
    return depth


def edges(board: Board) -> list[tuple[str, str, str]]:
    """(from step, to step, "output → input" names) for every handoff between steps, and
    "after" for an ordering edge (`after`), which carries nothing."""
    pairs: dict[tuple[str, str], list[str]] = {}
    for sid, b in board.blocks.items():
        for name in b.bindings:
            for r in b.refs(name):
                if r.step and r.step in board.blocks:
                    label = r.name if r.name == name else f"{r.name} → {name}"
                    pairs.setdefault((r.step, sid), []).append(label)
        for a in b.after:
            if a in board.blocks:
                pairs.setdefault((a, sid), []).append("after")
    return [(a, b, ", ".join(dict.fromkeys(ls))) for (a, b), ls in pairs.items()]


def answer_text(value: Any) -> str | None:
    """What a person chose, from an inbox answer {action, params?, values?, text?}."""
    if not isinstance(value, dict) or "action" not in value:
        return None
    for src in (value.get("values"), value.get("params")):
        if isinstance(src, dict) and src.get("value") not in (None, ""):
            return _short(src["value"], 200)
    if isinstance(value.get("text"), str) and value["text"].strip():
        return _line(value["text"], 200)
    return str(value["action"])


def _card(store: Store, board: Board, b: Block, live: bool) -> str:
    """A step on the board: a compact bubble with its status glyph, its id and, small, how long
    it ran (and `done of total` for a scattered step). Everything else is one click away in the
    drawer; the doc and what it says now (progress, error) are its tooltip."""
    tag = "a" if live else "div"
    href = f' href="{e(step_href(board.project, b.sid))}" data-step="{e(b.sid)}"' if live else ""
    kind, text = block_line(store, board, b)
    now = text if kind != "output" else ""  # what it produced is in the drawer
    if b.mark == "paused":
        now = f"paused: {b.pause_reason}" if b.pause_reason else "paused"
    tip = " — ".join(t for t in (" ".join(b.doc.split()), now) if t)
    title = f' title="{e(tip)}"' if tip else ""
    attrs = (f'class="node {"chip" if b.glue else "card"} is-{e(b.mark)}" id="n-{e(b.sid)}" '
             f'data-node="s:{e(b.sid)}"{href}{title}')
    small = []
    if "total" in b.entry:
        small.append(f"{int(b.entry.get('done') or 0)}/{int(b.entry['total'])}")
    if _elapsed(b):
        small.append(_elapsed(b))
    tail = f'<span class="dur">{" · ".join(small)}</span>' if small else ""
    return f'<{tag} {attrs}>{glyph(b.mark)}<span class="sid">{e(b.sid)}</span>{tail}</{tag}>'


def board_html(store: Store, board: Board, live: bool = True) -> str:
    """The plan as a board (the `graph` part): one row per dependency depth, top to bottom,
    inside the page's column. The server lays out the rows (the order reads without
    JavaScript); static/board.js draws the edges between the cards from `data-edges`."""
    if not board.blocks:
        return ('<p class="empty">No steps yet. The orchestrator adds them with '
                "<code>plan_patch</code>.</p>")
    depth = depths(board)
    rows: dict[int, list[str]] = {}
    for sid, b in board.blocks.items():
        rows.setdefault(depth[sid], []).append(_card(store, board, b, live))
    html_rows = "".join(
        f'<li class="row" style="--n:{min(len(cards), ROW_MAX)}">{"".join(cards)}</li>'
        for _, cards in sorted(rows.items()))
    data = json.dumps([[f"s:{a}", f"s:{b}", label] for a, b, label in edges(board)],
                      ensure_ascii=False)
    return (f'<div class="board" role="region" aria-label="Plan">'
            f'<div class="plane" data-edges="{e(data)}"><svg class="edges" aria-hidden="true">'
            f'</svg><ol class="rows">{html_rows}</ol></div></div>')


def result_panel(board: Board) -> str:
    """The plan's outputs once any has a value: what the whole plan produced."""
    rows = []
    for name, ref in board.plan.outputs.items():
        has, v = value_of(ref, board.plan, board.state)
        if has:
            rows.append(f"<div><dt>{e(name)}</dt><dd>{_result_value(v)}</dd></div>")
    if not rows:
        return ""
    return (f'<div class="result"><h2 class="label">Result</h2>'
            f'<dl class="kv">{"".join(rows)}</dl></div>')


def _result_value(value: Any) -> str:
    """A plan output: short values as they are; a long text folded to its first lines."""
    if isinstance(value, str) and ("\n" in value or len(value) > 200):
        body = markdown(value) if MARKDOWN_HINT.search(value) else f"<p>{e(value)}</p>"
        return (f'<details class="fold" data-preserve-attr="open"><summary>'
                f'<div class="md clip">{body}</div></summary></details>')
    return _value(value)


def inputs_strip(board: Board) -> str:
    """The plan inputs: name, value, and the doc under it."""
    if not board.plan.inputs:
        return ""
    rows = []
    for name, t in board.plan.inputs.items():
        doc = board.plan.input_docs.get(name, "")
        if name in board.state["inputs"]:
            v = _value(board.state["inputs"][name])
        elif isinstance(t, T.Optional):
            v = '<span class="quiet">null</span>'
        else:
            v = '<span class="attn">not set</span>'
        about = f'<p class="meta">{e(doc)}</p>' if doc else ""
        rows.append(f"<div><dt>{e(name)}</dt><dd>{v}{about}</dd></div>")
    return (f'<div class="inputs"><h2 class="label">Inputs</h2>'
            f'<dl class="kv">{"".join(rows)}</dl></div>')


def _about(text: str) -> str:
    """A description: two lines, and a disclosure to read the rest when it is longer."""
    if not text:
        return ""
    if len(text) <= 200 and "\n" not in text:
        return f'<p class="about">{e(text)}</p>'
    return (f'<details class="about" data-preserve-attr="open"><summary><span class="clamp">'
            f"{e(text)}</span></summary></details>")


def _summary_line(board: Board, updated: str = "") -> str:
    counts, total = board.counts, len(board.blocks)
    bits = [f"{counts.get('succeeded', 0)} of {total} succeeded"] if total else ["no steps"]
    bits += [f"{counts[s]} {s}" for s in ("running", "stale", "failed") if counts.get(s)]
    if board.cost is not None:
        bits.append(_money(board.cost))
    if updated:
        bits.append(f"updated {_when(updated)}")
    return " · ".join(bits)


# ---- the project index ------------------------------------------------------------------


def last_change(store: Store, project: str) -> str:
    """The later of the last log record and the last state.json write."""
    times = []
    last = L.last_record(store.log_dir(project))
    if last:
        times.append(last["at"])
    state = store.project_dir(project) / "state.json"
    if state.exists():
        t = dt.datetime.fromtimestamp(state.stat().st_mtime, dt.UTC)
        times.append(t.strftime("%Y-%m-%dT%H:%M:%SZ"))
    return max(times) if times else ""


def _bar(counts: Mapping[str, int], total: int) -> str:
    """Progress by status, proportional, with the same counts as text for assistive tech."""
    said = ", ".join(f"{counts[s]} {s}" for s in ("succeeded", "running", "stale", "failed",
                                                   "pending") if counts.get(s))
    segs = "".join(f'<i class="b-{s}" style="flex:{counts[s]}"></i>'
                   for s in ("succeeded", "running", "stale", "failed", "pending")
                   if counts.get(s))
    return f'<span class="bar" role="img" aria-label="{e(said or "no steps")}">{segs}</span>' \
        if total else '<span class="bar" role="img" aria-label="no steps"></span>'


def _project_row(store: Store, name: str) -> str:
    info = store.project(name)
    href = f"/projects/{quote(name)}"
    about = f'<p class="about">{e(info.get("description") or "")}</p>' \
        if info.get("description") else ""
    when = last_change(store, name)
    try:
        board = load_board(store, name)
    except SluiceError as err:  # a broken plan is shown, not raised
        msg = err.message
        return (f'<li class="proj"><div class="p-head"><a href="{href}">{e(name)}</a></div>'
                f'{about}<p class="now attn">The plan does not validate: {e(_line(msg, 120))}'
                f"</p></li>")
    counts, total = board.counts, len(board.blocks)
    running = [b for b in board.blocks.values() if b.status == "running"]
    if running:
        now = "".join(
            f'<li><a href="{href}#step:{quote(b.sid)}">{glyph("running")}'
            f'<span class="ttl">{e(b.title)}</span></a><span class="dur">{_elapsed(b)}</span>'
            f"</li>" for b in running[:4])
        more = f'<li class="more">and {len(running) - 4} more</li>' if len(running) > 4 else ""
        now = f'<ul class="now">{now}{more}</ul>'
    elif info.get("paused") is True:
        now = '<p class="now">Paused.</p>'
    elif total and counts.get("succeeded") == total:
        now = '<p class="now">Finished.</p>'
    elif counts.get("failed") or counts.get("stale"):
        now = '<p class="now">Stopped: nothing is running.</p>'
    elif total:
        now = '<p class="now">Nothing is running.</p>'
    else:
        now = '<p class="now">No steps yet.</p>'
    done = f"{counts.get('succeeded', 0)} of {total}" if total else ""
    return (f'<li class="proj"><div class="p-head"><a href="{href}">{e(name)}</a>'
            f'<span class="meta">{_when(when) if when else ""}</span></div>{about}'
            f'<div class="p-state">{_bar(counts, total)}<span class="meta">{done}</span></div>'
            f"{now}</li>")


def _index_needs(store: Store) -> str:
    """The compact "Needs you" lines of the index: one per project that has something."""
    rows = []
    for name in store.project_names():
        if store.archived(name):
            continue
        items = needs(store, name)
        if not items:
            continue
        kinds: dict[str, int] = {}
        for it in items:
            kinds[it["kind"]] = kinds.get(it["kind"], 0) + 1
        what = {"Answer": ("answer", "answers"), "Input": ("input", "inputs"),
                "Failed": ("failed step", "failed steps"), "Message": ("message", "messages")}
        text = " · ".join(f"{n} {what[k][n != 1]}" for k, n in kinds.items())
        rows.append(f'<li><a href="/projects/{quote(name)}"><span class="k">{e(name)}</span>'
                    f'<span class="t">{e(text)}</span></a></li>')
    if not rows:
        return ""
    return (f'<section class="needs" aria-labelledby="needs-h"><h2 class="label attn" '
            f'id="needs-h">Needs you</h2><ul>{"".join(rows)}</ul></section>')


def index_parts(store: Store) -> dict[str, str]:
    names = store.project_names()
    active = [n for n in names if not store.archived(n)]
    old = [n for n in names if n not in active]
    rows = "".join(_project_row(store, n) for n in active)
    body = f'<ul class="projects">{rows}</ul>' if rows else \
        ('<p class="empty">No projects yet. An orchestrator creates one with '
         "<code>project_create</code>.</p>" if not old else
         '<p class="empty">Every project is archived.</p>')
    if old:
        body += (f'<details class="archived" data-preserve-attr="open"><summary>Archived '
                 f'({len(old)})</summary><ul class="projects">'
                 f'{"".join(_project_row(store, n) for n in old)}</ul></details>')
    return {"needs": _part("needs", _index_needs(store)), "projects": _part("projects", body),
            "nav-inbox": nav_inbox(open_count(store))}


def index(store: Store, ver: str | None = None) -> str:
    """The project index; live (streaming from /stream) when given the home's version `ver`."""
    parts = index_parts(store)
    return layout("Projects", f'<h1 class="vh">Projects</h1>{parts["needs"]}'
                  f'{parts["projects"]}', stream="/stream" if ver else None,
                  signals={"ver": ver} if ver else None, inbox=open_count(store), here="/",
                  store=store,
                  board=bool(ver))


# ---- the project page -------------------------------------------------------------------


def project_parts(store: Store, project: str) -> dict[str, str]:
    """The live project page's parts by element id: what its stream patches."""
    return _project(store, project, True)


def _switch(action: str, field: str, on: bool, labels: tuple[str, str]) -> str:
    """A two-way switch posting `field` "1" or "0" to `action` (a plain form: it works
    without JavaScript); `labels` are (turn on, turn off)."""
    label, value = (labels[1], "0") if on else (labels[0], "1")
    return (f'<form class="switch" method="post" action="{e(action)}">'
            f'<input type="hidden" name="{field}" value="{value}">'
            f'<button type="submit">{label}</button></form>')


def _archive_form(project: str, archived: bool) -> str:
    return _switch(f"/projects/{quote(project)}/archive", "archived", archived,
                   ("Archive", "Unarchive"))


def _pause_form(project: str, paused: bool, sid: str | None = None) -> str:
    """Pause or resume the project, or with `sid` one step of it."""
    base = f"/projects/{quote(project)}" + (f"/steps/{quote(sid)}" if sid else "")
    return _switch(f"{base}/pause", "paused", paused, ("Pause", "Resume"))


def _project(store: Store, project: str, live: bool) -> dict[str, str]:
    board = load_board(store, project)
    about = board.info.get("description") or ""
    archived = board.info.get("archived") is True
    paused = board.info.get("paused") is True
    note = '<p class="attn-note">Paused: no step starts until you resume it.</p>' \
        if paused else ""
    note += '<p class="attn-note">Archived: listed apart and left out of Needs you.</p>' \
        if archived else ""
    line = f'<p class="meta sum">{_summary_line(board, last_change(store, project))}</p>'
    if live:
        line = (f'<div class="sumline">{line}<div class="switches">'
                f"{_pause_form(project, paused)}{_archive_form(project, archived)}</div></div>")
    # the head is the project: its description. What the plan took, produced and cost, and the
    # archive switch, sit under the board.
    about_plan = (result_panel(board) + inputs_strip(board) + line)
    parts = {"summary": _part("summary", note + _about(about)),
             "needs": _part("needs", needs_band(needs(store, project)) if live else ""),
             "graph": _part("graph", board_html(store, board, live)),
             "result": _part("result", about_plan, "section", "plan-facts")}
    if live:
        parts["nav-inbox"] = nav_inbox(open_count(store))
    return parts


def _drawer(project: str) -> str:
    """The step drawer: `$step` (from the address's `#step:<id>`) opens it and streams that
    step's detail into it; static/board.js keeps `$step` and the address in step."""
    url = f"'/projects/{quote(project)}/steps/' + encodeURIComponent($step) + '/stream'"
    effect = (f"$step ? @get({url}, {{retry: 'always', retryMaxCount: 1000000, "
              f"requestCancellation: window.sluiceStream ? window.sluiceStream() : 'auto'}}) "
              f": window.sluiceStream && window.sluiceStream()")
    hash_to_step = ("$step = location.hash.startsWith('#step:') ? "
                    "decodeURIComponent(location.hash.slice(6)) : ''")
    return (f'<div class="scrim" style="display:none" data-show="$step != \'\'" '
            f'data-on:click="window.sluiceClose && window.sluiceClose()"></div>'
            f'<aside id="drawer" class="drawer" style="display:none" tabindex="-1" '
            f'aria-label="Step" data-show="$step != \'\'" data-effect="{e(effect)}" '
            f'data-init="{e(hash_to_step)}" data-on:hashchange__window="{e(hash_to_step)}">'
            f'<button type="button" class="close" aria-label="Close" '
            f'data-on:click="window.sluiceClose && window.sluiceClose()">{X_ICON}</button>'
            f'<div id="step-detail"></div></aside>')


def project_page(store: Store, project: str, ver: str | None = None) -> str:
    """The project page: live (nav, links, the step drawer, streaming its changes) when given
    the project's version `ver`, else the standalone page plan_view returns (the board, then
    every step's detail in a disclosure)."""
    live = ver is not None
    p = _project(store, project, live)
    body = (f'{project_head(project, "plan" if live else None)}{p["summary"]}{p["needs"]}'
            f'{p["graph"]}{p["result"]}')
    if live:
        body += _drawer(project)
    else:
        board = load_board(store, project)
        body += "".join(
            f'<details class="std" id="step-{e(sid)}"><summary>{glyph(b.mark)}'
            f"<span>{e(b.title)}</span></summary>{step_detail(store, project, sid, False)}"
            f"</details>" for sid, b in board.blocks.items())
    return layout(project, body, nav=live,
                  stream=f"/projects/{project}/stream" if live else None,
                  signals={"ver": ver, "step": "", "sver": ""} if live else None,
                  inbox=open_count(store) if live else None, here="/", board=live,
                  store=store, project=project, tab="plan")


def render(store: Store, project: str, fmt: str) -> str:
    """plan_view: the Mermaid text or the standalone HTML page."""
    if fmt == "mermaid":
        plan = store.plan(project)[1]
        return mermaid(plan, store.read_state(project))
    if fmt == "html":
        return project_page(store, project)
    raise BadRequest(f'format must be "mermaid" or "html", got {fmt!r}')


# ---- a step's detail --------------------------------------------------------------------


def step_run_dirs(store: Store, project: str, sid: str) -> list[Path]:
    """The run directories of a step's current attempt (for change detection)."""
    entry = store.read_state(project)["steps"].get(sid) or {}
    return [d for r in entry.get("run_ids") or [] if isinstance(r, str)
            and (d := _run_dir(store, project, r)) is not None]


MARKDOWN_HINT = re.compile(r"^\s{0,3}(#{1,6} |[-*] |\d+\. |> )|\*\*|`[^`\n]+`", re.MULTILINE)


def _value(value: Any, long_at: int = 160) -> str:
    """A value: short text inline, markdown rendered, other long text (prose) or structures
    in a scrolling block, an inbox answer as what was chosen."""
    if (chosen := answer_text(value)) is not None:
        return f'<span class="v">{e(chosen)}</span>'
    if isinstance(value, str):
        if MARKDOWN_HINT.search(value):
            return f'<div class="v long md">{markdown(value)}</div>'
        if "\n" not in value and len(value) <= long_at:
            return f'<span class="v">{e(value)}</span>'
        return f'<div class="v long text">{e(value)}</div>'
    if isinstance(value, float):
        return f'<code class="v">{e(f"{value:.6g}")}</code>'
    text = json.dumps(value, ensure_ascii=False)
    if len(text) <= long_at:
        return f'<code class="v">{e(text)}</code>'
    return f'<pre class="v long">{_json(value)}</pre>'


def _runs(store: Store, project: str, sid: str) -> list[dict[str, Any]]:
    """The step's attempts from its step.status records: started, finished, outcome."""
    recs = L.read(store.log_dir(project), kinds=["step.status", "step.output"])["records"]
    runs: list[dict[str, Any]] = []
    for r in recs:
        if r.get("step") != sid:
            continue
        if r["kind"] == "step.output":
            runs.append({"started": r["at"], "finished": r["at"], "outcome": "set by hand",
                         "note": r.get("reason") or ""})
        elif r.get("to") == "running":
            runs.append({"started": r["at"], "outcome": "running"})
        elif r.get("to") in ("succeeded", "failed") and runs and runs[-1]["outcome"] == "running":
            runs[-1].update(finished=r["at"], outcome=r["to"],
                            note=_line(r.get("error") or "", 120))
    return runs


FOLD_LINES = 6  # a value longer than this folds, with "Show all"


def _fold(inner: str, cls: str) -> str:
    """A long value, folded to its first lines under a fade, "Show all" to open it."""
    return (f'<details class="fold {cls}" data-preserve-attr="open"><summary>'
            f"{inner}</summary></details>")


def field_value(value: Any) -> str:
    """A value in the step's detail: text as prose (markdown rendered), multi-line plain text
    and structures as code, an inbox answer as what was chosen; long ones fold."""
    if (chosen := answer_text(value)) is not None:
        return f'<span class="v">{e(chosen)}</span>'
    if isinstance(value, str):
        if MARKDOWN_HINT.search(value):
            body, cls, lines = markdown(value), "md", value.count("\n") + len(value) // 90
        elif "\n" in value:
            body, cls, lines = f"<pre>{e(value)}</pre>", "code", value.count("\n") + 1
        else:
            body, cls, lines = f"<p>{e(value)}</p>", "prose", len(value) // 90
        inner = f'<div class="clip {cls}">{body}</div>'
        return _fold(inner, cls) if lines > FOLD_LINES else f'<div class="v {cls}">{body}</div>'
    if isinstance(value, float):
        return f'<code class="v">{e(f"{value:.6g}")}</code>'
    if value is None:
        return '<span class="quiet">none</span>'
    text = json.dumps(value, ensure_ascii=False)
    if len(text) <= 80:
        return f'<code class="v">{e(text)}</code>'
    pretty = _json(value)
    inner = f'<div class="clip code"><pre>{pretty}</pre></div>'
    return _fold(inner, "code") if pretty.count("\n") > FOLD_LINES else \
        f'<div class="v code"><pre>{pretty}</pre></div>'


def _from(project: str, block: Block, name: str, live: bool) -> str:
    """Where a binding comes from, as a small link (nothing for a value set in the plan)."""
    out = []
    for r in block.refs(name):
        if r.step:
            ref = e(str(r))
            out.append(f'<a href="{e(step_href(project, r.step))}" data-step="{e(r.step)}">'
                       f"{ref}</a>" if live else ref)
        else:
            out.append(f"input {e(str(r))}")
    return f'<span class="f-from">← {", ".join(out)}</span>' if out else ""


def _field(name: str, value: str, type_: str = "", doc: str = "", source: str = "") -> str:
    """One named value: its name (its type shown with Types on, and in the name's title), where
    it comes from, its doc and its value."""
    t = f' title="{e(type_)}"' if type_ else ""
    ty = f'<span class="f-type">{e(type_)}</span>' if type_ else ""
    about = f'<p class="f-doc">{e(doc)}</p>' if doc else ""
    return (f'<div class="f"><div class="f-k"><span class="f-name"{t}>{e(name)}</span>{ty}'
            f"{source}</div>{about}<div class=\"f-v\">{value}</div></div>")


def step_detail(store: Store, project: str, sid: str, live: bool = True) -> str:
    """Everything about one step, the way a run history reads: the step and a summary of its
    run (status, fn, started, duration, cost, session), then what matters now (error,
    progress), what it produced, its messages, its prompt and other inputs (where each comes
    from), its log output and, when it ran more than once, its attempts. Types show on demand
    (the Types switch; always in a name's title). The `step-detail` part of the drawer and of
    the step page."""
    board = load_board(store, project)
    b = board.blocks.get(sid)
    if b is None:
        raise NotFound(f"the plan of project {project} has no step {sid!r}")
    outs_all = b.entry.get("outputs") if isinstance(b.entry.get("outputs"), dict) else {}
    facts = [("Status", e(WORDS.get(b.mark, b.status))), ("Function", f"<code>{e(b.fn)}</code>")]
    if b.after:
        facts.append(("After", ", ".join(f"<code>{e(a)}</code>" for a in b.after)))
    if b.tags:
        facts.append(("Tags", ", ".join(e(t) for t in b.tags)))
    if "total" in b.entry:
        facts.append(("Runs", f"{int(b.entry.get('done') or 0)} of {int(b.entry['total'])}"))
    if b.entry.get("started"):
        facts.append(("Started", _when(b.entry["started"])))
    if _elapsed(b):
        facts.append(("Duration", _elapsed(b)))
    if b.cost is not None:
        facts.append(("Cost", e(_money(b.cost))))
    session = outs_all.get("session")
    if isinstance(session, str) and session:
        facts.append(("Session", f'<code title="{e(session)}">{e(session[:8])}</code>'))
    doc = f'<p class="d-doc">{e(" ".join(b.doc.split()))}</p>' if b.doc.strip() else ""
    if b.paused and b.status != "running":
        doc += f'<p class="d-doc attn-note">Paused{": " + e(b.pause_reason) if b.pause_reason else ""}</p>'
    grid = "".join(f"<div><dt>{k}</dt><dd>{v}</dd></div>" for k, v in facts)
    switch = f'<div class="d-actions">{_pause_form(project, b.paused, sid)}' \
        "</div>" if live else ""
    head = (f'<header class="d-head"><div class="hd">{glyph(b.mark)}<h2>{e(sid)}</h2></div>'
            f'{doc}<dl class="facts">{grid}</dl>{switch}</header>')
    sections = []

    def section(title: str, body: str, extra: str = "") -> None:
        sections.append(f'<section class="d-sec"><div class="d-sec-h">{_label(title)}{extra}'
                        f"</div>{body}</section>")

    types_switch = ('<button type="button" class="types-toggle" aria-pressed="false" '
                    'title="Show the types of the values">Types</button>')
    if b.entry.get("error"):
        section("Error", f'<pre class="err">{e(b.entry["error"])}</pre>')
    tail = ""
    if b.run_ids:
        d = _run_dir(store, project, b.run_ids[-1])
        tail = tail_text(d / "stderr.log", TAIL).strip() if d else ""
    which = f" (run {len(b.run_ids)} of {int(b.entry['total'])})" \
        if "total" in b.entry and len(b.run_ids) > 1 else ""
    if b.status == "running":
        section("Progress" + which, f'<pre class="tail">{e(tail)}</pre>' if tail else
                '<p class="quiet">Nothing written yet.</p>')
    # outputs: what it produced (its declared ones first); session and cost are run facts
    outs: dict[str, Any] | None = outs_all if isinstance(b.entry.get("outputs"), dict) else None
    declared = b.outputs
    title = "Outputs"
    raw = b.raw.get("outputs")
    own = {n: t for n, t in declared.items() if isinstance(raw, dict) and n in raw}
    if outs is None and b.status == "running" and b.run_ids:
        d = _run_dir(store, project, b.run_ids[-1])
        try:  # what the agent has submitted so far (step_submit), before the fn exits
            got = read_json(d / "submitted.json") if d else None
        except (OSError, ValueError):
            got = None
        if isinstance(got, dict):
            outs, title, declared = got, "Outputs submitted so far", own
    if outs is not None:
        fields = [_field(n, field_value(outs[n]) if n in outs else
                         '<span class="quiet">none</span>', declared.get(n, ""),
                         b.output_docs.get(n, ""))
                  for n in dict.fromkeys([*declared, *outs]) if n not in RUN_FACTS]
        if fields:
            section(title, f'<div class="fields">{"".join(fields)}</div>', types_switch)
    elif own:
        names = ", ".join(e(n) for n in own)
        section("Outputs", f'<p class="quiet">None yet. It hands on: {names}.</p>')
    thread = f"step-{sid}"
    msgs = L.read(store.log_dir(project), kinds=["message"], threads=[thread])["records"]
    if msgs:
        items = "".join(
            f'<li><p class="meta">{e(str(m.get("from") or "?"))}'
            + (f" → {e(str(m['to']))}" if m.get("to") else "")
            + f' · {_when(m.get("at", ""))}</p><div class="msg">{e(str(m.get("body", "")))}</div>'
              f"</li>" for m in msgs)
        log = f"/projects/{quote(project)}/log?thread={quote(thread)}"
        section("Messages", f'<ul class="msgs">{items}</ul>'
                + (f'<p class="more"><a href="{log}">Thread in the log</a></p>' if live else ""))
    # the run's own input.json when there is one, else what the bindings resolve to now
    ran: dict[str, Any] = {}
    if b.run_ids:
        d = _run_dir(store, project, b.run_ids[-1])
        try:
            got = read_json(d / "input.json") if d else None
            ran = got if isinstance(got, dict) else {}
        except (OSError, ValueError):
            ran = {}
    prompt = next((n for n in PROMPT_INPUTS if n in b.bindings), None)

    def resolved(name: str) -> tuple[bool, Any]:
        if name in ran:
            return True, ran[name]
        refs = b.refs(name)
        if not refs:
            return name in b.bindings, b.default(name)
        vals = [value_of(r, board.plan, board.state) for r in refs]
        src = b.bindings[name]["source"]
        if isinstance(src, list):
            return all(ok for ok, _ in vals), [v for _, v in vals]
        return vals[0]

    if prompt:
        ok, v = resolved(prompt)
        text = v if isinstance(v, str) else json.dumps(v)
        body = (_fold(f'<div class="clip prose prompt">{e(text)}</div>', "prose")
                if ok and text.count("\n") + len(text) // 90 > FOLD_LINES else
                f'<div class="prompt">{e(text)}</div>' if ok else
                '<p class="quiet">Not resolved yet.</p>')
        section(prompt.capitalize(), _from(project, b, prompt, live) + body)
    fields = []
    for n in b.bindings:
        if n == prompt:
            continue
        ok, v = resolved(n)
        fields.append(_field(n, field_value(v) if ok else '<span class="quiet">no value yet</span>',
                             b.fn_inputs.get(n, ""), source=_from(project, b, n, live)))
    if fields:
        section("Inputs", f'<div class="fields">{"".join(fields)}</div>', types_switch)
    if tail and b.status != "running":
        lines = tail.splitlines()
        body = f'<pre class="tail">{e(tail)}</pre>'
        if len(lines) > FOLD_LINES:
            body = (f'<details data-preserve-attr="open"><summary>Show {len(lines)} lines'
                    f"</summary>{body}</details>")
        section("Log output" + which, body)
    runs = _runs(store, project, sid)
    if len(runs) > 1:
        items = []
        for i, r in enumerate(reversed(runs)):
            start, end = _parse_iso(r.get("started")), _parse_iso(r.get("finished"))
            took = dur((end - start).total_seconds()) if start and end else ""
            note = f'<span class="quiet">{e(r["note"])}</span>' if r.get("note") else ""
            items.append(f'<li><span class="a-n">{len(runs) - i}</span>'
                         f'<span class="a-o a-{e(r["outcome"].split()[0])}">{e(r["outcome"])}'
                         f'</span>{note}<span class="a-t">{_when(r["started"])}'
                         f'{" · " + e(took) if took else ""}</span></li>')
        section("Attempts", f'<ol class="attempts">{"".join(items)}</ol>')
    return f'{head}{"".join(sections)}'


def step_parts(store: Store, project: str, sid: str) -> dict[str, str]:
    return {"step-detail": _part("step-detail", step_detail(store, project, sid))}


def step_page(store: Store, project: str, sid: str, ver: str) -> str:
    """One step on a page of its own (what a card links to without JavaScript)."""
    parts = step_parts(store, project, sid)
    return layout(f"{sid} · {project}", project_head(project, "plan") + parts["step-detail"],
                  stream=f"/projects/{project}/steps/{sid}/stream", signals={"sver": ver},
                  inbox=open_count(store), here="/", sub=True, board=True, store=store,
                  project=project, tab="plan")


# ---- the log viewer ---------------------------------------------------------------------


def _split(values: Iterable[Any]) -> list[str]:
    return list(dict.fromkeys(x.strip() for v in values for x in str(v).split(",")
                              if x.strip()))


def _seq_param(name: str, values: list[Any]) -> int | None:
    raw = str(values[-1]).strip() if values else ""
    if raw in ("", "0"):
        return None
    if not raw.isdigit():
        raise BadRequest(f"{name}: expected a seq (a positive integer), got {raw!r}")
    return int(raw)


@dataclasses.dataclass(frozen=True)
class LogQuery:
    """What a log page shows: the §6b filter (`kinds`, `threads`) and where the page is
    (`before`/`after` a seq, or neither for the newest records)."""

    kinds: tuple[str, ...] = ()
    threads: tuple[str, ...] = ()
    before: int | None = None
    after: int | None = None

    @classmethod
    def parse(cls, params: Mapping[str, list[Any]]) -> LogQuery:
        """From query parameters (`kind` and `thread` repeated or comma-separated, `before`,
        `after`); raises BadRequest."""
        kinds = _split(params.get("kind", []))
        errs = L.check_kinds(kinds)
        if errs:
            raise BadRequest("; ".join(errs))
        before = _seq_param("before", params.get("before", []))
        after = _seq_param("after", params.get("after", []))
        if before is not None and after is not None:
            raise BadRequest("give before or after, not both")
        return cls(tuple(k for k in KIND_OPTIONS if k in kinds),
                   tuple(_split(params.get("thread", []))), before, after)

    @classmethod
    def from_signals(cls, signals: Mapping[str, Any]) -> LogQuery:
        """From the log page's Datastar signals (the same parser as the query string)."""
        kinds = signals.get("kinds")
        return cls.parse({"kind": [k for k in kinds if k] if isinstance(kinds, list) else [],
                          "thread": [signals.get("thread") or ""],
                          "before": [signals.get("before") or ""],
                          "after": [signals.get("after") or ""]})

    @property
    def newest(self) -> bool:
        return self.before is None and self.after is None

    def query(self, **change: Any) -> str:
        """The canonical query string, with `change`d fields."""
        q = dataclasses.replace(self, **change)
        items = [("kind", k) for k in q.kinds]
        if q.threads:
            items.append(("thread", ",".join(q.threads)))
        if q.before is not None:
            items.append(("before", str(q.before)))
        if q.after is not None:
            items.append(("after", str(q.after)))
        return urlencode(items)


def _st(status: Any) -> str:
    return f'<span class="s-{e(str(status))}">{e(str(status))}</span>'


def log_summary(rec: dict[str, Any]) -> str:
    """One line (HTML) saying what a log record is about."""
    kind = rec.get("kind")
    by = f"rev {rec.get('rev')} by {rec.get('author') or '?'}"
    if rec.get("reason"):
        by += f": {_line(rec['reason'], 80)}"
    if kind == "step.status":
        text = e(f"{rec.get('step')} {rec.get('from') or 'new'} → ") + _st(rec.get("to"))
        return text + (e(": " + _line(rec["error"])) if rec.get("error") else "")
    if kind == "plan.edit":
        return e(f"{by} ({len(rec.get('ops') or [])} ops)")
    if kind == "plan.input":
        value = _line(json.dumps(rec.get("value"), ensure_ascii=False), 60)
        return e(f"{rec.get('name')} = {value} · {by}")
    if kind == "step.output":
        forced = " (forced)" if rec.get("force") else ""
        return e(f"{rec.get('step')} set by hand{forced} · {by}")
    if kind == "step.submit":
        outs = rec.get("outputs")
        names = ", ".join(map(str, outs)) if isinstance(outs, dict) and outs else "nothing"
        return e(f"{rec.get('step')} submitted {names}")
    if kind == "step.retry":
        return e(f"{rec.get('step')} retried · {by}")
    if kind == "call":
        text = e(f"{rec.get('call')} {rec.get('fn')} ") + _st(rec.get("status"))
        return text + (e(": " + _line(rec["error"])) if rec.get("error") else "")
    if kind == "message":
        to = f" → {rec['to']}" if rec.get("to") else ""
        return e(f"{rec.get('thread')} from {rec.get('from')}{to}: {_line(rec.get('body', ''))}")
    if kind == "inbox.post":
        who = f" from {rec['from']}" if rec.get("from") else ""
        return e(f"{rec.get('item')}{who}: {_line(rec.get('title', ''))}")
    if kind == "inbox.answer":
        answer = rec.get("answer") or {}
        text = f": {_line(answer['text'], 60)}" if answer.get("text") else ""
        return e(f"{rec.get('item')} answered by {rec.get('by')} ({answer.get('action')}){text}")
    if kind == "inbox.close":
        why = f": {_line(rec['reason'], 80)}" if rec.get("reason") else ""
        return e(f"{rec.get('item')} closed by {rec.get('by')}{why}")
    return e(_line(json.dumps(rec, ensure_ascii=False)))


def log_row(rec: dict[str, Any]) -> str:
    at = str(rec.get("at", ""))
    return (f'<tr id="r{int(rec["seq"])}"><td class="num">{int(rec["seq"])}</td>'
            f'<td class="at"><time datetime="{e(at)}" title="{e(at)}">{e(at[11:19] or at)}'
            f'</time></td><td class="kind">{e(str(rec.get("kind", "")))}</td>'
            f'<td><details data-preserve-attr="open"><summary>{log_summary(rec)}</summary>'
            f"<pre>{_json(rec)}</pre></details></td></tr>")


def log_rows(records: Iterable[dict[str, Any]]) -> str:
    return "".join(map(log_row, records))


def _log_table(records: list[dict[str, Any]], body_id: str | None = None) -> str:
    """Records (newest first) as a table; an empty one says so."""
    if not records and body_id is None:
        return '<p class="quiet">No records yet.</p>'
    tbody = f'<tbody id="{body_id}">' if body_id else "<tbody>"
    return ('<div class="scroll"><table class="log"><thead><tr><th class="num">seq</th>'
            '<th class="at">time</th><th>kind</th><th>what</th></tr></thead>'
            f"{tbody}{log_rows(records)}</tbody></table></div>")


def log_base(project: str | None) -> str:
    return f"/projects/{project}/log" if project else "/log"


def log_view(store: Store, project: str | None, q: LogQuery) -> tuple[str, int]:
    """The `log-view` part: one page of records (newest first) and the pager; with the log's
    last seq when it was read."""
    res = L.page(store.log_dir(project), q.kinds, q.threads, q.before, q.after, PAGE_SIZE)
    recs, base = res["records"], log_base(project)

    def link(text: str, **change: Any) -> str:
        qs = q.query(**change)
        return f'<a href="{e(base + ("?" + qs if qs else ""))}">{text}</a>'

    pager = []
    if not q.newest:
        pager.append(link("« newest", before=None, after=None))
    if res["newer"] and recs:
        pager.append(link("‹ newer", before=None, after=recs[0]["seq"]))
    if res["older"] and recs:
        pager.append(link("older ›", before=recs[-1]["seq"], after=None))
    body = ('<p class="quiet log-empty">No matching records.</p>'
            + _log_table(recs, "log-rows")
            + f'<nav class="pager">{" ".join(pager)}</nav>')
    return f'<div id="log-view" class="log-view">{body}</div>', res["last_seq"]


def log_page(store: Store, project: str | None, q: LogQuery) -> str:
    """The log viewer: a filter form (a plain GET form; with Datastar, changing it updates
    the table and the address bar in place) over `log_view`. The newest page streams new
    records in at the top."""
    if project is not None:
        store.project(project)
    view, last = log_view(store, project, q)
    base = log_base(project)
    apply = f"$before = 0; $after = 0; @get('{base}/stream', {STREAM_OPTIONS})"
    boxes = "".join(
        f'<label><input type="checkbox" name="kind" value="{k}" data-bind:kinds'
        f'{" checked" if k in q.kinds else ""}> {k}</label>' for k in KIND_OPTIONS)
    form = (f'<form class="filters" method="get" action="{e(base)}" '
            f'data-on:input__debounce.300ms="{e(apply)}" data-on:submit="{e(apply)}">'
            f"<fieldset><legend>Kinds</legend>{boxes}</fieldset>"
            f'<label class="thread">Threads <input name="thread" '
            f'value="{e(",".join(q.threads))}" placeholder="any" size="16" data-bind:thread>'
            f"</label><button>Apply</button></form>")
    if project is None:
        title = '<h1 class="vh">Log</h1><p class="meta">Calls made without a project.</p>'
    else:
        history = bool(q.kinds) and set(q.kinds) == set(L.HISTORY_KINDS) and not q.threads
        tab = "history" if history else "log"
        title = project_head(project, tab)
    signals = {"kinds": [k if k in q.kinds else "" for k in KIND_OPTIONS],
               "thread": ",".join(q.threads), "before": q.before or 0, "after": q.after or 0,
               "view": q.query(), "seen": last}
    url = "history.replaceState(null, '', location.pathname + ($view ? '?' + $view : ''))"
    return layout(f"{project or 'home'} log", f"{title}{form}{view}",
                  stream=f"{base}/stream", signals=signals,
                  main_attrs=f' data-effect="{e(url)}"', inbox=open_count(store),
                  here="/log" if project is None else "/",
                  store=store, project=project,
                  tab=None if project is None else tab)


# ---- functions --------------------------------------------------------------------------


def type_text(form: Any) -> str:
    """A CWL type form, readably: `string[]`, `enum(a|b)`, `{field: type}`, `T?`."""
    if isinstance(form, str):
        return form
    if isinstance(form, list) and len(form) == 2 and "null" in form:
        return type_text(form[1] if form[0] == "null" else form[0]) + "?"
    if isinstance(form, dict):
        kind = form.get("type")
        if kind == "array":
            return type_text(form.get("items")) + "[]"
        if kind == "enum" and isinstance(form.get("symbols"), list):
            return "enum(" + "|".join(map(str, form["symbols"])) + ")"
        if kind == "record" and isinstance(form.get("fields"), dict):
            return "{" + ", ".join(f"{k}: {type_text(v)}" for k, v in form["fields"].items()) + "}"
    return json.dumps(form)


def _ports(ports: Any) -> str:
    if not isinstance(ports, dict) or not ports:
        return '<span class="quiet">none</span>'
    return "<br>".join(f"{e(k)}: <code>{e(type_text(v))}</code>" for k, v in ports.items())


def fns_page(store: Store, project: str | None = None) -> str:
    reg = store.registry(project)
    groups: dict[str, list[str]] = {}
    for x in reg.listing():
        cls = "fn problem" if x.get("error") else "fn"
        err = f'<p class="err">{e(x["error"])}</p>' if x.get("error") else ""
        groups.setdefault(x["scope"], []).append(
            f'<div class="{cls}"><div class="fn-head"><b>{e(x["name"])}</b> '
            f'<span class="quiet">{e(x.get("doc") or "")}</span></div>{err}'
            f'<div class="ports"><div><span class="label">Inputs</span>'
            f"<p>{_ports(x.get('inputs'))}</p></div><div><span class=\"label\">Outputs</span>"
            f"<p>{_ports(x.get('outputs'))}</p></div></div></div>")
    other = [p for p in reg.problems if not p["where"].endswith("fn.json")]  # e.g. a missing dir
    options = "".join(f'<option value="{e(n)}"{" selected" if n == project else ""}>{e(n)}'
                      f"</option>" for n in store.project_names())
    picker = (f'<form class="picker" method="get" action="/fns"><label>As seen by '
              f'<select name="project" onchange="this.form.submit()"><option value="">'
              f"no project</option>{options}</select></label><noscript><button>Show"
              f"</button></noscript></form>")
    sections = []
    for scope in ("builtin", "global", "project"):
        if scope == "project" and project is None:
            continue
        title = SCOPE_TITLES[scope] + (f" ({e(project)})" if scope == "project" else "")
        cards = "".join(groups.get(scope, [])) or '<p class="quiet">none</p>'
        sections.append(f"<h2>{title}</h2>{cards}")
    extra = "".join(f'<p class="fn problem err">{e(p["where"])}: {e(p["message"])}</p>'
                    for p in other)
    title = '<h1 class="vh">Functions</h1>' if project is None else project_head(project, "fns")
    return layout("Functions", f"{title}{picker}{extra}{''.join(sections)}",
                  inbox=open_count(store), here="/fns" if project is None else "/",
                  store=store, project=project, tab="fns")


# ---- the inbox --------------------------------------------------------------------------


def inbox_base(project: str | None) -> str:
    return f"/projects/{project}/inbox" if project else "/inbox"


def _item(item: dict[str, Any], back: str, all_projects: bool) -> str:
    """One item: title, where it comes from, its markdown body, then its answer box (open) or
    its answer or close reason."""
    p, iid = item["project"], item["id"]
    meta = [f'<a href="/projects/{e(p)}/inbox">{e(p)}</a>' if all_projects else "",
            f"from {e(item['from'])}" if item.get("from") else "",
            f"asked {_when(item['created'])}",
            f"sets <code>{e(item['input'])}</code>" if item.get("input") else "",
            f'<span class="quiet">{e(iid)}</span>']
    out = [f'<h3>{e(item["title"])}</h3>',
           f'<p class="meta">{" · ".join(m for m in meta if m)}</p>']
    if item.get("body"):
        out.append(f'<div class="md">{markdown(item["body"])}</div>')
    url = f"/projects/{p}/inbox/{iid}/answer"
    if item["status"] == "open":
        ui = f' data-ui="{e(item["ui"])}"' if item.get("ui") else ""
        out.append(
            f'<div class="answer" data-ignore-morph data-url="{e(url)}" '
            f'data-key="{e(p)}/{e(iid)}"{ui}><form method="post" action="{e(url)}">'
            f'<input type="hidden" name="next" value="{e(back)}">'
            f'<textarea name="text" rows="3" required aria-label="answer"></textarea>'
            f'<button class="primary">Answer</button></form></div>')
    elif item["status"] == "answered":
        answer = item.get("answer") or {}
        text = f"<blockquote>{e(answer['text'])}</blockquote>" if answer.get("text") else ""
        out.append(f'<div class="answered"><p class="meta">answered '
                   f'{_when(item.get("answered", ""))} with <code>'
                   f'{e(str(answer.get("action")))}</code></p>{text}'
                   f'<details><summary>Answer as sent</summary><pre>{_json(answer)}</pre>'
                   f"</details></div>")
    else:
        why = f": {e(item['reason'])}" if item.get("reason") else ""
        out.append(f'<p class="meta">closed {_when(item.get("closed", ""))}{why}</p>')
    return f'<article class="item" id="item-{e(p)}-{e(iid)}">{"".join(out)}</article>'


def _waiting(store: Store, project: str | None) -> str:
    """Unset plan inputs that hold up a step (and no open item asks for): read-only, since a
    value comes through an inbox item or plan_set_input."""
    rows = [f'<tr><td><a href="/projects/{e(w["project"])}">{e(w["project"])}</a></td>'
            f'<td><code>{e(w["name"])}</code></td><td><code>{e(w["type"])}</code></td>'
            f'<td>{e(w.get("doc", ""))}</td><td>{e(", ".join(w["steps"]))}</td></tr>'
            for w in store.waiting_inputs(project)]
    if not rows:
        return ""
    return ('<h2>Waiting on a person</h2><p class="quiet">Plan inputs with no value; set one '
            "with <code>plan_set_input</code>, or post an item with its input.</p>"
            '<div class="scroll"><table><tr><th>project</th><th>input</th><th>type</th>'
            f'<th>doc</th><th>steps waiting</th></tr>{"".join(rows)}</table></div>')


def inbox_parts(store: Store, project: str | None, status: str) -> dict[str, str]:
    """The inbox page's parts: its items (open ones oldest first, the rest newest first), on
    the open view the plan inputs waiting on a person, and the nav badge."""
    items = store.inbox(project, status)
    items = items if status == "open" else items[::-1]
    back = inbox_base(project) + ("" if status == "open" else f"?status={status}")
    empty = {"open": "Nothing is waiting on you."}.get(status, f"No {status} items.")
    body = "".join(_item(i, back, project is None) for i in items) \
        or f'<p class="empty">{e(empty)}</p>'
    if status == "open":
        body += _waiting(store, project)
    return {"inbox-items": _part("inbox-items", body),
            "nav-inbox": nav_inbox(open_count(store), project is None)}


def inbox_page(store: Store, project: str | None, status: str, ver: str) -> str:
    """The inbox (every project's, or one project's): a status filter over `inbox_parts`,
    streaming its changes; open items draw their OpenUI program with /static/inbox.js."""
    if project is not None:
        store.project(project)
    if status not in INBOX_FILTERS:
        raise BadRequest(f"status: expected one of {', '.join(INBOX_FILTERS)}, got {status!r}")
    base, parts = inbox_base(project), inbox_parts(store, project, status)
    filters = "".join(
        f'<a href="{e(base + ("" if s == "open" else "?status=" + s))}"'
        f'{_current(s, status)}>{s.capitalize()}</a>' for s in INBOX_FILTERS)
    title = '<h1 class="vh">Inbox</h1>' if project is None else project_head(project, "inbox")
    return layout("Inbox", f'{title}<nav class="seg" aria-label="Status">{filters}</nav>'
                  f'{parts["inbox-items"]}', stream=f"{base}/stream",
                  signals={"ver": ver, "status": status}, inbox=open_count(store),
                  script="/static/inbox.js", here="/inbox", store=store, project=project)
