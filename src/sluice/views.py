"""Views (SPEC §8): the HTML of the dashboard `sluice serve` renders, and `plan_view`.

- `mermaid(plan, state)`: the plan as a Mermaid flowchart (plan_view's text format, for agents).
- `index`: every project as one row. What waits on a person is the inbox alone (its count is
  the nav's coral badge); nothing else asks for them.
- `project_page`: the project's description, then its plan as a board of cards laid out by
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
import functools
import html
import json
import re
import signal
from collections.abc import Callable, Iterable, Mapping
from pathlib import Path
from typing import Any
from urllib.parse import quote, urlencode

from markdown_it import MarkdownIt

from . import log as L
from . import state as S
from . import types as T
from .errors import BadRequest, NotFound, SluiceError
from .plan import Plan, Ref, Source, Step, source_value, value_of
from .store import Store
from .util import read_json, tail_text

# the dashboard's palette (static/dashboard.css, light), one class per status; failed is ink
# with a heavy border, never coral (coral is the inbox badge's alone)
CLASSES = {"pending": "fill:#f5eede,stroke:#788190,color:#46587a",
           "running": "fill:#e1ebf8,stroke:#1f81fa,color:#0d2b67",
           "succeeded": "fill:#e3eedb,stroke:#11813c,color:#0d2b67",
           "failed": "fill:#fdf8ec,stroke:#0d2b67,stroke-width:3px,color:#0d2b67",
           "stale": "fill:#f6ebcf,stroke:#916100,color:#0d2b67",
           "skipped": "fill:#fdf8ec,stroke:#788190,color:#46587a,stroke-dasharray:3 3",
           "manual": "fill:#fdf8ec,stroke:#11813c,stroke-width:3px,stroke-dasharray:6 3"}
# Datastar with Rocket (web components), served from static/ like every script the dashboard
# runs; static/sluice.js imports the same module
DATASTAR_JS = "/static/datastar-rocket-1.0.4.js"
# Archivo (weight and width axes) for display, Public Sans for text; the system sans without them
FONT_CSS = ("https://cdn.jsdelivr.net/npm/@fontsource-variable/archivo@5.3.0/wdth.css",
            "https://cdn.jsdelivr.net/npm/@fontsource-variable/public-sans@5.3.0/index.css")
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
QUIET = 15 * 60  # seconds without a write before a running step has gone quiet

STATIC = Path(__file__).resolve().parent / "static"
CSS = (STATIC / "dashboard.css").read_text(encoding="utf-8")

e = html.escape
# CommonMark plus tables; raw HTML is escaped as text and unsafe link schemes are refused.
MARKDOWN = MarkdownIt("commonmark", {"html": False}).enable("table")
INBOX_FILTERS = ("open", "answered", "closed", "all")


def markdown(text: str) -> str:
    """Rendered markdown whose top heading is an h4: under the page's own h1, the drawer's h2
    and its h3 labels, so a spec's `# Title` does not claim the page outline."""
    tokens = MARKDOWN.parse(text)
    heads = [t for t in tokens if t.type in ("heading_open", "heading_close")]
    shift = 4 - min((int(t.tag[1]) for t in heads), default=4)
    for t in heads:
        t.tag = f"h{min(6, int(t.tag[1]) + shift)}"
    return MARKDOWN.renderer.render(tokens, MARKDOWN.options, {})


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
        entry = S.entry_of(state, sid)
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
        entry = S.entry_of(state, sid)
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
    """A relative time the page keeps current (`data-ago`, static/sluice.js)."""
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
    "skipped": _RING + ' stroke-dasharray="2.6 2.2"/><path d="M5.3 10.7l5.4-5.4" fill="none" '
               'stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>',
}
WORDS = {"manual": "set by hand", "paused": "paused"}
X_ICON = ('<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path '
          'd="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.5" '
          'stroke-linecap="round"/></svg>')


def glyph(status: str, sep: str = "") -> str:
    """The status glyph with its word for assistive technology (then `sep`, e.g. ", ", so a
    link that starts with it reads "failed, a" rather than "failed a")."""
    word = WORDS.get(status, status)
    return (f'<span class="g g-{e(status)}" title="{e(word)}"><svg viewBox="0 0 16 16" '
            f'width="16" height="16" aria-hidden="true">{GLYPHS.get(status, GLYPHS["pending"])}'
            f'</svg><span class="vh">{e(word + sep)}</span></span>')


# ---- layout -----------------------------------------------------------------------------


def open_count(store: Store) -> int:
    """How many inbox items wait on a person, across every project."""
    return len(store.inbox())


# the Inbox's icon, shown in place of its word on a phone
TRAY = ('<svg class="tray" viewBox="0 0 20 20" width="20" height="20" aria-hidden="true">'
        '<path d="M3 11.5 5 4.5h10l2 7M3 11.5V15.5h14v-4M3 11.5h4l1 2h4l1-2h4" fill="none" '
        'stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>')


def nav_inbox(count: int | None, current: bool = False) -> str:
    """The nav's Inbox link: its count of open items is the dashboard's one coral badge."""
    badge = f' <span class="badge" title="open items">{count}</span>' if count else ""
    cur = ' aria-current="page"' if current else ""
    return f'<a id="nav-inbox" href="/inbox"{cur}>{TRAY}<span class="t">Inbox</span>{badge}</a>'


NAV = (("/log", "Log"), ("/fns", "Functions"))  # with no project chosen; "/" is the switcher's
# The brand: the owner's mark (static/logo.svg) and the wordmark as live text beside it.
BRAND_MARK = ('<img class="mark" src="/static/logo.svg" width="27" height="26" alt="">'
              '<span class="wordmark" aria-hidden="true">sluice</span>')
CHEVRON = ('<svg class="chev" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">'
           '<path d="M4.5 6.5 8 10l3.5-3.5" fill="none" stroke="currentColor" stroke-width="1.6" '
           'stroke-linecap="round" stroke-linejoin="round"/></svg>')
PROJECT_TABS = ("plan", "threads", "log", "history", "fns")  # a project's sections, in the nav


def project_head(project: str, tab: str | None) -> str:
    """A project page's title: the nav already names the project (its switcher) and the
    section, so it is for assistive technology only; visible on the standalone page
    (`tab` None), which has no nav."""
    if tab is None:
        return f'<div class="phead"><h1>{e(project)}</h1></div>'
    return f'<h1 class="vh">{e(project)}</h1>'


def _project_status(counts: Mapping[str, int]) -> str:
    """One status for a whole project: running, failed, stale, finished, or waiting."""
    total = sum(counts.values())
    for status in ("running", "failed", "stale"):
        if counts.get(status):
            return status
    done = counts.get("succeeded", 0) + counts.get("skipped", 0)
    return "succeeded" if total and done == total else "pending"


def _project_mark(counts: Mapping[str, int]) -> str:
    """One glyph for a whole project (the switcher's menu)."""
    return glyph(_project_status(counts), ", ")


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
    Threads, Log, History, Functions) or of all of them (Log, Functions; the switcher's "All
    projects" is the index), and the Inbox with the one coral badge."""
    if project is not None:
        p = quote(project)
        hrefs = {"plan": (f"/projects/{p}", "Plan"),
                 "threads": (f"/projects/{p}/threads", "Threads"),
                 "log": (f"/projects/{p}/log", "Log"),
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
           tab: str | None = None, skip: tuple[str, str] | None = None, tail: str = "") -> str:
    """A page. With `stream`, Datastar opens that SSE stream once the page has loaded (with
    `signals`, the page's Datastar signals, sent along as the `datastar` query parameter).
    `inbox` is the count of open items for the nav's badge; `script` a module to load; `here`
    the nav entry of this page, `project` and `tab` the chosen project and its section (`sub`:
    a page inside that entry, as a step is inside Plan); `store` lists the projects for the
    nav's switcher; `board` loads static/sluice.js (times, and the board, drawer and thread
    components). `skip` is a (target id, text) link past the nav, the first thing a keyboard
    reaches; `tail` goes after `main` (the step drawer, which must stay reachable when the
    page is inert behind it)."""
    head = f'<script type="module" src="{DATASTAR_JS}"></script>' if stream else ""
    scripts = "".join(f'<script type="module" src="{e(s)}"></script>'
                      for s in (script, "/static/sluice.js" if board else "",
                                "/static/nav.js" if nav else "") if s)
    top = top_nav(store, project, tab, here, inbox, sub) if nav else ""
    body_attrs = f' data-signals="{_signals(signals)}"' if signals else ""
    if skip:
        top = f'<a class="skip" href="#{e(skip[0])}">{e(skip[1])}</a>{top}'
    if stream:
        main_attrs += f' data-init="@get(\'{e(stream)}\', {STREAM_OPTIONS})"'
    return (f'<!doctype html>\n<html lang="en"><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width,initial-scale=1">'
            f"<title>{e(title)} · sluice</title>"
            f'<link rel="icon" href="/static/favicon.svg" type="image/svg+xml">'
            + "".join(f'<link rel="stylesheet" href="{u}">' for u in FONT_CSS)
            + f"<style>{CSS}</style>{head}</head>\n"
            f"<body{body_attrs}>{top}<main{main_attrs}>\n{body}\n</main>{tail}{scripts}"
            "</body></html>\n")


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
    """One step as the dashboard shows it: the plan's parsed Step (plan.py reads its
    bindings, `when`, pause and tags) plus its state.json entry."""

    step: Step
    entry: dict[str, Any]
    glue: bool  # a built-in that runs inline (core.*): a slim chip on the board
    fn_inputs: dict[str, str]  # its fn's inputs, then any extra ones it binds (open fns)
    fn_outputs: dict[str, str]  # its declared outputs first (open fns), then its fn's
    output_docs: dict[str, str] = dataclasses.field(default_factory=dict)
    submitted: frozenset[str] = frozenset()  # its outputs the agent submits (declared, submits)

    @property
    def sid(self) -> str:
        return self.step.id

    @property
    def fn(self) -> str:
        return self.step.fn.name

    @property
    def doc(self) -> str:
        return self.step.doc

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
        return self.step.paused

    @property
    def pause_reason(self) -> str:
        return self.step.pause_reason.strip()

    @property
    def after(self) -> list[str]:
        return self.step.after

    @property
    def when(self) -> Ref | None:
        return self.step.when

    @property
    def tags(self) -> list[str]:
        return self.step.tags

    @property
    def title(self) -> str:
        return " ".join(self.doc.split()) or self.sid

    @property
    def bindings(self) -> dict[str, Source]:
        return self.step.sources

    def refs(self, name: str) -> list[Ref]:
        """The refs one binding reads (none for a default)."""
        src = self.step.sources.get(name)
        return list(src.refs) if src is not None else []

    @property
    def waits(self) -> list[str]:
        """The steps it waits for: those it reads from, then those it runs after."""
        return self.step.waits

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

    @functools.cached_property
    def held(self) -> dict[str, list[str]]:
        """Each pending step a failure holds up: its id → the failed steps upstream of it,
        directly or through other pending steps (paused ones too), in plan order."""
        memo: dict[str, set[str]] = {}

        def up(sid: str) -> set[str]:
            if sid not in memo:
                memo[sid] = set()  # (the plan is acyclic; this only guards the recursion)
                out: set[str] = set()
                for d in self.blocks[sid].waits:
                    b = self.blocks.get(d)
                    if b is None:
                        continue
                    if b.status == "failed":
                        out.add(d)
                    elif b.status == "pending":
                        out |= up(d)
                memo[sid] = out
            return memo[sid]

        order = list(self.blocks)
        return {sid: sorted(up(sid), key=order.index) for sid, b in self.blocks.items()
                if b.status == "pending" and up(sid)}

    def blocked(self, sid: str) -> bool:
        """A pending step a failed step holds up that is not paused itself (a paused one is
        counted as paused)."""
        return sid in self.held and self.blocks[sid].mark == "pending"

    def blocks_of(self, failed: str) -> list[str]:
        """The pending steps a failed step holds up, in plan order."""
        return [sid for sid, ups in self.held.items() if failed in ups]

    @property
    def stuck(self) -> dict[str, int]:
        """How many pending steps wait on a failure (`blocked`) or are paused (`paused`)."""
        paused = sum(b.mark == "paused" for b in self.blocks.values())
        return {"blocked": sum(map(self.blocked, self.blocks)), "paused": paused}

    @property
    def failed(self) -> list[str]:
        return [sid for sid, b in self.blocks.items() if b.status == "failed"]


def load_board(store: Store, project: str) -> Board:
    info = store.project(project)
    doc, plan = store.plan(project)
    state = store.read_state(project)
    blocks = {}
    for sid, step in plan.steps.items():
        declared = step.declared
        extra = step.extra
        blocks[sid] = Block(
            step, S.entry_of(state, sid), step.fn.native,
            {k: str(v) for k, v in {**step.fn.inputs, **extra}.items()},
            {k: str(v) for k, v in {**declared, **step.fn.outputs}.items()},
            dict(step.output_docs), frozenset(step.declared))
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


def _activity(store: Store, project: str, block: Block) -> tuple[str, float] | None:
    """(iso, seconds since) a running step's last sign of life: the newest mtime of the
    stderr.log (the run dir's own when there is none) of each run that has not finished (no
    exit.json). None for a step that is not running or has no live run."""
    if block.status != "running":
        return None
    times = []
    for r in block.run_ids:
        d = _run_dir(store, project, r)
        if d is None or (d / "exit.json").exists():
            continue
        try:
            times.append((d / "stderr.log").stat().st_mtime)
        except OSError:
            try:
                times.append(d.stat().st_mtime)
            except OSError:
                pass
    if not times:
        return None
    act = dt.datetime.fromtimestamp(max(times), dt.UTC)
    return act.strftime("%Y-%m-%dT%H:%M:%SZ"), (_now() - act).total_seconds()


def quiet_mark(store: Store, project: str, block: Block, after: bool) -> str:
    """A running step's `quiet 42m` once it has written nothing for QUIET seconds (` · ` first
    when it comes `after` something), kept current by the ticker (static/sluice.js) from
    `data-quiet`; '' for a step with no live run."""
    act = _activity(store, project, block)
    if act is None:
        return ""
    text = f"{' · ' if after else ''}quiet {dur(act[1])}" if act[1] >= QUIET else ""
    return f'<span class="quiet" data-quiet="{e(act[0])}">{e(text)}</span>'


def quiet_since(store: Store, board: Board) -> list[str]:
    """When each running step of the board last wrote (quiet or not): the tab title counts
    those gone quiet, as the page ages."""
    acts = (_activity(store, board.project, b) for b in board.blocks.values())
    return [a[0] for a in acts if a is not None]


def quiet_text(age: float, line: str) -> str:
    """A quiet running step's "what it says now" line: `Quiet for 42m. Last output: …`."""
    return f"Quiet for {dur(age)}. " + (f"Last output: {line}" if line else "No output yet.")


EXC_CLASS = re.compile(r"^(?:[A-Za-z_]\w*\.)*[A-Z]\w*(?:Error|Exception|Exit|Interrupt|Failure)"
                       r":\s+(?=\S)")
EXIT_CODE = re.compile(r"\b(exited|exit (?:code|status)|non-zero exit status)( -?\d+)\b(?! \()")
SIGNAL_WORDS = {"SIGTERM": "terminated", "SIGKILL": "killed", "SIGINT": "interrupted",
                "SIGHUP": "hung up", "SIGQUIT": "quit", "SIGABRT": "aborted",
                "SIGSEGV": "crashed", "SIGBUS": "crashed", "SIGPIPE": "broken pipe"}


def exit_signal(code: int) -> str:
    """What a command's exit code says about a signal that ended it: `terminated: SIGTERM`
    for 143 (128 + n, or -n from Python), else ""."""
    n = code - 128 if code > 128 else -code
    try:
        name = signal.Signals(n).name
    except ValueError:
        return ""
    return f"{SIGNAL_WORDS.get(name, 'signalled')}: {name}"


def error_headline(error: Any, width: int = 200) -> str:
    """What went wrong, in one line and in sluice's words: an error's last non-empty line (a
    traceback, or a command's output, ends with the exception) without the exception's class,
    the home directory as `~`, an exit code that means a signal explained (`exited 143
    (terminated: SIGTERM)`), at most `width` characters. The whole error is shown apart."""
    lines = [ln.strip() for ln in str(error or "").splitlines() if ln.strip()]
    if not lines:
        return ""
    line = EXC_CLASS.sub("", lines[-1])
    home = str(Path.home()).rstrip("/")
    if home:
        line = re.sub(rf"{re.escape(home)}(?=/|\b|$)", "~", line)

    def why(m: re.Match[str]) -> str:
        said = exit_signal(int(m[2]))
        return f"{m[0]} ({said})" if said else m[0]
    return _line(EXIT_CODE.sub(why, line), width)


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
        line = progress_line(store, board.project, block)
        act = _activity(store, board.project, block)
        if act is not None and act[1] >= QUIET:
            return "progress", quiet_text(act[1], line)
        return "progress", line
    if status == "failed":
        return "error", error_headline(block.entry.get("error")) or "failed"
    if status == "stale":
        return "note", "Its inputs changed since it ran"
    if status == "skipped":
        return "note", "Skipped: " + (block.entry.get("skipped") or "")
    if status == "succeeded":
        return "output", output_summary(block)
    missing = _missing_inputs(board, block)
    if missing:
        return "note", "Waits for " + ", ".join(missing)
    return "", ""


# ---- messages -------------------------------------------------------------------------


def _awaiting(msgs: list[dict[str, Any]], blocks: Mapping[str, Any]) -> list[dict[str, Any]]:
    """The questions still open: messages that ask for a reply (`needs_reply`, true unless
    the sender marked a note), addressed to someone other than a step of the plan (the
    orchestrator, a person), with no later message from that addressee on the same thread.
    On a step's thread, only while that step is in the plan and not finished: once it has
    succeeded, failed or been skipped (or left the plan), nobody is waiting on the answer."""
    out = []
    for i, m in enumerate(msgs):
        to = m.get("to")
        if not to or to in blocks or m.get("needs_reply") is False:
            continue
        t = str(m.get("thread") or "")
        if t.startswith("step-") and (t[5:] not in blocks or blocks[t[5:]].status
                                      in ("succeeded", "failed", "skipped")):
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


MSG_FOLD_LINES = 8


def _body_html(body: str) -> str:
    """A message body: markdown when it looks like markdown, else paragraphs with their line
    breaks; a long one folds."""
    if MARKDOWN_HINT.search(body):
        html = markdown(body)
    else:
        html = "".join(f"<p>{e(p).replace(chr(10), '<br>')}</p>"
                       for p in body.strip().split("\n\n") if p.strip())
    lines = body.count("\n") + len(body) // 90
    inner = f'<div class="clip md m-body">{html}</div>'
    return _fold(inner, "md") if lines > MSG_FOLD_LINES else inner


def message_html(m: dict[str, Any], steps: Iterable[str], awaiting: bool) -> str:
    """One message: who to whom and when, whether it waits on a reply or is only a note, then
    the body. A step's messages sit on the left, everyone else's (the orchestrator) indented."""
    sender, to = str(m.get("from") or "?"), m.get("to")
    who = "step" if sender in set(steps) else "lead"
    tag = ('<span class="m-tag await">Awaiting reply</span>' if awaiting else
           '<span class="m-tag">note</span>' if m.get("needs_reply") is False else "")
    head = (f'<div class="m-head"><span class="m-from">{e(sender)}</span>'
            + (f'<span class="m-to">→ {e(str(to))}</span>' if to else "")
            + f'<span class="m-when">{_when(m.get("at", ""))}</span>{tag}</div>')
    return (f'<li class="m m-{who}" data-seq="{e(str(m.get("seq", "")))}">{head}'
            f'{_body_html(str(m.get("body", "")))}</li>')


def threads_href(project: str, thread: str) -> str:
    return f"/projects/{quote(project)}/threads#th-{quote(thread)}"


THREAD_KEEP = 3  # a thread's last messages shown; the earlier ones fold


def thread_html(board: Board, thread: str, ms: list[dict[str, Any]], waiting: set[int],
                live: bool) -> str:
    """One thread as a <sluice-thread>: a summary (the step it belongs to, how many messages,
    the last one), then its messages, the earlier ones folded (from its first open question
    on, none are). Open when a question on it waits for a reply. The component marks what
    arrived since this browser last opened it."""
    sid = _thread_step(thread, board.blocks)
    if sid:
        b = board.blocks[sid]
        name = f'{glyph(b.mark)}<span class="th-name">{e(sid)}</span>'
        name += f'<span class="th-doc">{e(b.title)}</span>' if b.doc.strip() else ""
    elif thread.startswith("step-"):
        name = (f'<span class="th-name">{e(thread[5:])}</span>'
                f'<span class="th-doc">no longer in the plan</span>')
    else:
        name = f'<span class="th-name">{e(thread)}</span>'
    open_q = [i for i, m in enumerate(ms) if m["seq"] in waiting]
    tag = f'<span class="m-tag await">{len(open_q)} awaiting reply</span>' if open_q else ""
    last = ms[-1]
    preview = (f'<span class="th-last"><b>{e(str(last.get("from") or "?"))}:</b> '
               f'{e(_line(str(last.get("body", "")), 160))}</span>')
    count = f'{len(ms)} message{"s" if len(ms) != 1 else ""}'
    summary = (f'<summary><span class="th-top">{name}<span class="th-meta">{count}'
               f'<span class="th-when"> · {_when(last.get("at", ""))}</span></span>'
               f'<span class="th-new" data-ignore-morph></span>{tag}</span>{preview}</summary>')
    cut = max(0, min([len(ms) - THREAD_KEEP, *open_q]))
    items = [message_html(m, board.blocks, m["seq"] in waiting) for m in ms]
    body = ""
    if cut:
        body += (f'<details class="earlier" data-preserve-attr="open"><summary>{cut} earlier '
                 f'message{"s" if cut != 1 else ""}</summary><ol class="msgs">'
                 f'{"".join(items[:cut])}</ol></details>')
    body += f'<ol class="msgs">{"".join(items[cut:])}</ol>'
    if sid and live:
        body += (f'<p class="more"><a href="/projects/{e(quote(board.project))}#step:'
                 f'{e(quote(sid))}">Open {e(sid)} on the plan</a></p>')
    return (f'<sluice-thread project="{e(board.project)}" thread="{e(thread)}" '
            f'last="{last["seq"]}" data-preserve-attr="class data-rocket-host">'
            f'<details class="thread" id="th-{e(thread)}" data-preserve-attr="open"'
            f'{" open" if open_q else ""}>{summary}{body}</details></sluice-thread>')


def threads_panel(store: Store, board: Board, live: bool = True) -> str:
    """Every conversation of the project (the `threads` part of the Threads tab), the latest
    first."""
    msgs = L.read(store.log_dir(board.project), kinds=["message"])["records"]
    if not msgs:
        return ('<p class="empty">No messages yet. Agents running as steps post to their '
                "thread (<code>step-&lt;id&gt;</code>) and the orchestrator answers there.</p>")
    waiting = {m["seq"] for m in _awaiting(msgs, board.blocks)}
    threads: dict[str, list[dict[str, Any]]] = {}
    for m in msgs:
        threads.setdefault(str(m.get("thread") or ""), []).append(m)
    order = sorted(threads, key=lambda t: threads[t][-1]["seq"], reverse=True)
    return (f'<div class="threads">'
            f'{"".join(thread_html(board, t, threads[t], waiting, live) for t in order)}</div>')


def threads_parts(store: Store, project: str) -> dict[str, str]:
    board = load_board(store, project)
    return {"threads": _part("threads", threads_panel(store, board)),
            "nav-inbox": nav_inbox(open_count(store))}


def threads_page(store: Store, project: str, ver: str) -> str:
    """The Threads tab: the project's conversations, live."""
    parts = threads_parts(store, project)
    return layout(f"Threads · {project}",
                  f'<h1 class="vh">Threads · {e(project)}</h1>{parts["threads"]}',
                  stream=f"/projects/{project}/threads/stream", signals={"ver": ver},
                  inbox=open_count(store), here="/", board=True, store=store,
                  project=project, tab="threads")


# ---- the board: lanes of rows by dependency depth ----------------------------------------

RUN_FACTS = ("session", "cost_usd")  # what a run says about itself, not what it produced
SWEEPS = 4  # ordering passes down and up a lane


def depths(board: Board) -> dict[str, int]:
    """Each step's row: 0 for a step that waits on no other step, else one below its deepest
    upstream."""
    depth: dict[str, int] = {}

    def row_of(sid: str) -> int:
        if sid not in depth:
            depth[sid] = 0  # (the plan is acyclic; this only guards the recursion)
            depth[sid] = max((row_of(d) + 1 for d in board.blocks[sid].waits
                              if d in board.blocks), default=0)
        return depth[sid]

    for sid in board.blocks:
        row_of(sid)
    return depth


def lanes(board: Board) -> tuple[list[dict[int, list[str]]], dict[str, int]]:
    """The board's lanes, left to right, each {row: step ids left to right}, and each step's
    row. A lane is the steps joined by handoffs (edges that carry a value; `after` only
    orders), so independent pieces of work stand side by side instead of interleaving. Lanes
    keep the plan's order; inside one, each row is sorted by where its neighbours sit
    (a few sweeps down and up), which undoes most crossings."""
    depth = depths(board)
    ids = list(board.blocks)
    parent = {sid: sid for sid in ids}

    def root(sid: str) -> str:
        while parent[sid] != sid:
            parent[sid] = parent[parent[sid]]
            sid = parent[sid]
        return sid

    up: dict[str, list[str]] = {sid: [] for sid in ids}
    down: dict[str, list[str]] = {sid: [] for sid in ids}
    for a, b, label in edges(board):
        up[b].append(a)
        down[a].append(b)
        if label != "after":
            parent[root(a)] = root(b)
    groups: dict[str, list[str]] = {}
    for sid in ids:
        groups.setdefault(root(sid), []).append(sid)
    out = []
    for members in groups.values():
        rows: dict[int, list[str]] = {}
        for sid in members:
            rows.setdefault(depth[sid], []).append(sid)
        pos = {sid: (i + .5) / len(r) for r in rows.values() for i, sid in enumerate(r)}
        order = sorted(rows)
        for _ in range(SWEEPS):
            for d in order[1:]:
                _by_neighbours(rows[d], up, pos)
            for d in reversed(order[:-1]):
                _by_neighbours(rows[d], down, pos)
        out.append(rows)
    return out, depth


def _boxes(board: Board, groups: list[dict[int, list[str]]]) -> list[list[int]]:
    """The lanes (indexes into `groups`) of each independent piece of work: lanes joined by
    any edge (only an `after` can join two lanes) share a box. Boxes and their lanes keep the
    plan's order."""
    lane = {sid: i for i, rows in enumerate(groups) for r in rows.values() for sid in r}
    parent = list(range(len(groups)))

    def root(i: int) -> int:
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    for a, b, _ in edges(board):
        parent[root(lane[a])] = root(lane[b])
    boxes: dict[int, list[int]] = {}
    for i in range(len(groups)):
        boxes.setdefault(root(i), []).append(i)
    return list(boxes.values())


ROOM = 960 - 36  # px a row of a box has: the column, less the box's padding
CARD_GAP, LANE_GAP = 14, 36  # px between cards in a row, and before the next lane's first


def _card_width(board: Board, b: Block) -> float:
    """About how wide a step's card is drawn (px): its id in 14.5px Inter, and in 12px what
    it says small (blocked, runs done, its time)."""
    small = " · ".join(t for t in (
        "blocked" if board.blocked(b.sid) else "",
        f"{b.entry.get('done') or 0}/{b.entry['total']}" if "total" in b.entry else "",
        re.sub(r"<[^>]+>", "", _elapsed(b))) if t)
    return 50 + 7.7 * len(b.sid) + (8 + 6.7 * len(small) if small else 0)


def _ups(board: Board, groups: list[dict[int, list[str]]],
         box: list[int]) -> tuple[dict[str, int], dict[int, list[tuple[str, str]]]]:
    """Each step's lane in a box, and the edges into each lane from the box's other lanes."""
    lane = {sid: i for i in box for r in groups[i].values() for sid in r}
    ups: dict[int, list[tuple[str, str]]] = {i: [] for i in box}
    for a, b, _ in edges(board):
        if a in lane and b in lane and lane[a] != lane[b]:
            ups[lane[b]].append((a, b))
    return lane, ups


def _shifts(board: Board, groups: list[dict[int, list[str]]], box: list[int],
            depth: dict[str, int], room: float = ROOM) -> dict[int, int]:
    """How many rows each lane of a box moves down, so that each lane's cards stay together:
    a lane that would crowd a row it shares past the box's width (`room`) starts below the
    lanes placed before it instead of wrapping in among their rows. Lanes are placed in the
    box's order, and none ever sits above a step it runs after. All lanes stay put when that
    cannot hold (an `after` cycle between lanes)."""
    lane, ups = _ups(board, groups, box)
    shift: dict[int, int] = {}

    def least(i: int) -> int:
        return max((depth[a] + shift.get(lane[a], 0) + 1 - depth[b] for a, b in ups[i]),
                   default=0)

    used: dict[int, float] = {}  # each row's width so far
    for i in box:
        wide = {d: sum(_card_width(board, board.blocks[sid]) for sid in r)
                + CARD_GAP * (len(r) - 1) for d, r in groups[i].items()}
        k, clear = max(least(i), 0), max(used, default=-1) + 1 - min(wide)
        while k < clear and any(used.get(d + k, -LANE_GAP) + LANE_GAP + w > room
                                for d, w in wide.items()):
            k += 1
        shift[i] = k
        for d, w in wide.items():
            used[d + k] = used.get(d + k, -LANE_GAP) + LANE_GAP + w
    for _ in box:  # a lane placed early may hang from one moved after it
        moved = [i for i in box if least(i) > shift[i]]
        if not moved:
            return shift
        for i in moved:
            shift[i] = least(i)
    return dict.fromkeys(box, 0)


def _seats(board: Board, groups: list[dict[int, list[str]]], box: list[int],
           shift: dict[int, int], at: list[int]) -> dict[int, list[int]]:
    """The lanes in each row of a box (`at`), left to right. A lane keeps the side of the
    row it stood on in the rows above (it takes the free place nearest its last one, before
    the lanes starting there do); a lane starting takes the free place nearest the steps it
    hangs from. So a lane does not jump across the box when another ends beside it."""
    _, ups = _ups(board, groups, box)
    place: dict[str, float] = {}  # each step's place across its row, 0 to 1
    seat: dict[int, float] = {}  # each lane's place in the last row it stood in
    out: dict[int, list[int]] = {}
    for v in at:
        here = [i for i in box if v - shift[i] in groups[i]]
        free = [(k + .5) / len(here) for k in range(len(here))]
        want = {}
        for i in here:
            xs = [place[a] for a, b in ups[i] if a in place and b in groups[i][v - shift[i]]]
            want[i] = seat.get(i, sum(xs) / len(xs) if xs else .5)
        for i in sorted(here, key=lambda i: (i not in seat, want[i])):
            seat[i] = min(free, key=lambda f: abs(f - want[i]))
            free.remove(seat[i])
        out[v] = sorted(here, key=seat.__getitem__)
        cards = [sid for i in out[v] for sid in groups[i][v - shift[i]]]
        place.update((sid, (k + .5) / len(cards)) for k, sid in enumerate(cards))
    return out


def _by_neighbours(row: list[str], near: dict[str, list[str]], pos: dict[str, float]) -> None:
    """Sort a row by the mean place (0-1 across their row) of each step's neighbours in its
    lane (`pos` holds the lane's steps), then record the new places."""
    def key(sid: str) -> float:
        xs = [pos[n] for n in near[sid] if n in pos]
        return sum(xs) / len(xs) if xs else pos[sid]
    row.sort(key=key)
    for i, sid in enumerate(row):
        pos[sid] = (i + .5) / len(row)


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
        if b.when is not None and b.when.step in board.blocks:
            pairs.setdefault((b.when.step, sid), []).append(f"when {b.when.name}")
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


def waits_on(board: Board, b: Block) -> list[tuple[str, str]]:
    """(step, status) of each step a pending one still waits for: those not yet succeeded or
    skipped."""
    if b.status != "pending":
        return []
    return [(d, board.blocks[d].mark) for d in b.waits
            if d in board.blocks and board.blocks[d].status not in ("succeeded", "skipped")]


def is_next(board: Board, b: Block) -> bool:
    """A pending step that starts as soon as the steps it waits for, all running now,
    finish: the board sets it apart from pending steps further off."""
    waits = waits_on(board, b)
    return (b.mark == "pending" and bool(waits)
            and all(board.blocks[d].status == "running" for d, _ in waits))


def _waits_text(waits: list[tuple[str, str]]) -> str:
    return "waits on " + ", ".join(f"{d} ({WORDS.get(m, m)})" for d, m in waits)


def _card(store: Store, board: Board, b: Block, live: bool, lane_start: bool = False,
          order: int | None = None, lane_top: bool = False) -> str:
    """A step on the board: a compact bubble with its status glyph, its id and, small, how long
    it ran (and `done of total` for a scattered step). Everything else is one click away in the
    drawer; the doc and what it says now (progress, error, what it waits on) are its tooltip.
    A pending step next in line (`is-next`) reads at full strength; one a failed step holds up
    (`is-blocked`) says "blocked". Its accessible name reads "failed, a, 1h 14m"."""
    tag = "a" if live else "div"
    href = f' href="{e(step_href(board.project, b.sid))}" data-step="{e(b.sid)}"' if live else ""
    kind, text = block_line(store, board, b)
    now = text if kind != "output" else ""  # what it produced is in the drawer
    if b.mark == "paused":
        now = f"paused: {b.pause_reason}" if b.pause_reason else "paused"
    elif waits_on(board, b) and not now:
        now = _waits_text(waits_on(board, b))
    tip = " — ".join(t for t in (" ".join(b.doc.split()), now) if t)
    title = f' title="{e(tip)}"' if tip else ""
    nxt = " is-next" if is_next(board, b) else ""
    nxt += " is-blocked" if board.blocked(b.sid) else ""
    attrs = (f'class="node {"chip" if b.glue else "card"} is-{e(b.mark)}{nxt}'
             f'{" lane-start" if lane_start else ""}{" lane-top" if lane_top else ""}"'
             f'{"" if order is None else f' style="--o:{order}"'} '
             f'id="n-{e(b.sid)}" data-node="s:{e(b.sid)}"{href}{title}')
    small = ["blocked"] if board.blocked(b.sid) else []
    if "total" in b.entry:
        small.append(f"{int(b.entry.get('done') or 0)}/{int(b.entry['total'])}")
    if _elapsed(b):
        small.append(_elapsed(b))
    inner = " · ".join(small) + quiet_mark(store, board.project, b, bool(small))
    tail = f'<span class="dur">{inner}</span>' if inner else ""
    # a comma for the accessible name, inline in the id's box (a hidden box of its own would
    # read "a , 1h")
    sep = '<span class="sep">,</span>' if inner else ""
    return (f'<{tag} {attrs}>{glyph(b.mark, ", ")}<span class="sid">{e(b.sid)}{sep}</span>'
            f"{tail}</{tag}>")


LEGEND_DATA = ('<svg width="22" height="8" aria-hidden="true"><path d="M1 4h20" '
               'stroke="var(--edge-head)" stroke-width="1.5"/></svg>')
LEGEND_AFTER = ('<svg width="22" height="8" aria-hidden="true"><path d="M1 4h20" '
                'stroke="var(--edge-head)" stroke-width="1.5" stroke-dasharray="4 4"/></svg>')


def board_html(store: Store, board: Board, live: bool = True) -> str:
    """The plan as a board (the `graph` part): each independent piece of work (the steps any
    edge joins, handoff or `after`) its own box when there are several, the boxes wrapping.
    A box is rows by dependency depth from its first step; in a row, its cards stand grouped
    by lane (the steps joined by handoffs), lane by lane, and a row too wide wraps within
    itself. A box of several steps that have all succeeded (or were skipped) is folded to one
    line, a <details> that opens to its cards. The server lays the cards out (the order reads
    without JavaScript); the <sluice-board> component (static/sluice.js) draws the edges
    between them from its `edges` attribute, around the cards they would cross."""
    if not board.blocks:
        return ('<p class="empty">No steps yet. The orchestrator adds them with '
                "<code>plan_patch</code>.</p>")
    groups, depth = lanes(board)
    boxes = _boxes(board, groups)
    html = []
    for box in boxes:
        shift = _shifts(board, groups, box, depth, ROOM if len(boxes) > 1 else ROOM + 36)
        at = sorted({d + shift[i] for i in box for d in groups[i]})  # the box's rows
        seats = _seats(board, groups, box, shift, at)
        top = at[0]
        rows = []
        for v in at:
            cards = []
            for i in seats[v]:
                n, d = box.index(i), v - shift[i]
                for k, sid in enumerate(groups[i].get(d, [])):
                    # the first card of the next lane in this row marks where it begins; on a
                    # phone the box stacks its lanes one after another (`--o`: lane, then row)
                    cards.append(_card(store, board, board.blocks[sid], live,
                                       lane_start=k == 0 and bool(cards),
                                       order=n * 1000 + v - top if len(box) > 1 else None,
                                       lane_top=n > 0 and k == 0 and d == min(groups[i])))
            rows.append(f'<li class="row" style="--r:{v - top + 1}">{"".join(cards)}</li>')
        inner = (f'<ol class="rows" style="--rows:{at[-1] - top + 1}">'
                 f'{"".join(rows)}</ol>')
        ids = [sid for v in at for i in seats[v] for sid in groups[i][v - shift[i]]]
        if len(boxes) > 1 and len(ids) > 1 and _done(board, ids):
            html.append(f'<li class="box done">{_folded(board, ids, inner)}</li>')
        else:
            html.append(f'<li class="box">{inner}</li>')
    es = edges(board)
    data = json.dumps([[f"s:{a}", f"s:{b}", label] for a, b, label in es], ensure_ascii=False)
    legend = ""
    if es:
        after = any("after" in label.split(", ") for _, _, label in es)
        legend = (f'<p class="legend">{LEGEND_DATA}hands on a value'
                  + (f"{LEGEND_AFTER}runs after" if after else "") + "</p>")
    return (f'<sluice-board class="board" role="region" aria-label="Plan" edges="{e(data)}" '
            f'data-preserve-attr="data-rocket-host"><div class="plane">'
            f'<svg class="edges" aria-hidden="true" data-ignore-morph></svg>'
            f'<ol class="boxes{" boxed" if len(boxes) > 1 else ""}">{"".join(html)}</ol>'
            f"</div>{legend}</sluice-board>")


def _done(board: Board, ids: list[str]) -> bool:
    """Whether a box's work is finished: every step succeeded (by hand too), or was skipped
    while the rest succeeded."""
    marks = {board.blocks[sid].status for sid in ids}
    return marks <= {"succeeded", "skipped"} and "succeeded" in marks


def _folded(board: Board, ids: list[str], inner: str) -> str:
    """A finished box folded to one line: its first and last steps, how many, and how they
    ended; it opens to its cards (open across live updates, and per tab in sessionStorage)."""
    skipped = sum(board.blocks[sid].status == "skipped" for sid in ids)
    ended = f"{len(ids) - skipped} succeeded, {skipped} skipped" if skipped else "all succeeded"
    # on a phone the first id takes the line and the count goes under it; the last id hides
    return (f'<details class="fold-box" data-preserve-attr="open" data-box="{e(ids[0])}">'
            f'<summary>{glyph("succeeded", ", ")}<span class="sid">{e(ids[0])}</span>'
            f'<span class="fb-meta"><span class="fb-last"><span aria-hidden="true"> … </span>'
            f'<span class="vh"> to </span>{e(ids[-1])}<span class="fb-dot"> · </span></span>'
            f'<span class="fb-n">{len(ids)} steps · {ended}</span></span>{CHEVRON}</summary>'
            f"{inner}</details>")


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
    """A description: short, as it is; longer, as markdown folded to its first lines, with a
    disclosure for the rest."""
    if not text:
        return ""
    if len(text) <= 200 and "\n" not in text:
        return f'<p class="about">{e(text)}</p>'
    lines = min(3, max(1, -(-len(first_paragraph(text)) // 80)))  # its opening, about
    return (f'<details class="about" data-preserve-attr="open"><summary><div class="clip md" '
            f'style="--lines:{lines}">{markdown(text)}</div></summary></details>')


def first_paragraph(text: str) -> str:
    """A description's opening, as plain text for one line of a list: up to its first blank
    line or list, whitespace collapsed."""
    head = re.split(r"\n\s*\n|\n\s*(?:[-*]|\d+\.)\s", text.strip(), maxsplit=1)[0]
    return " ".join(head.split())


def _summary_line(board: Board, updated: str = "") -> str:
    """Succeeded of total, then what else there is: skipped, running, stale, failed, and of
    the pending steps those a failure blocks and those paused; the cost, the last activity."""
    counts, total = {**board.counts, **board.stuck}, len(board.blocks)
    bits = [f"{counts.get('succeeded', 0)} of {total} succeeded"] if total else ["no steps"]
    bits += [f"{counts[s]} {s}" for s in ("skipped", "running", "stale", "failed", "blocked",
                                          "paused") if counts.get(s)]
    if board.cost is not None:
        bits.append(_money(board.cost))
    if updated:
        bits.append(f"updated {_when(updated)}")
    return " · ".join(bits)


def attention(board: Board, link: Callable[[str], str], drawer: bool = False,
              mark: bool = True) -> str:
    """The line a project with failed steps leads with: which steps failed (each a link, from
    `link(sid)`; with `drawer`, one that opens the step drawer; after the glyph unless not
    `mark`), how many pending steps they block, how many are paused; "Stopped:" first when
    nothing is running. Failures are the orchestrator's to retry, so this reports and does not
    ask (the inbox asks). Empty for a project with none."""
    failed, stuck = board.failed, board.stuck
    if not failed:
        return ""
    names = [f'<a href="{e(link(sid))}"{f' data-step="{e(sid)}"' if drawer else ""}>{e(sid)}</a>'
             for sid in failed[:2]]
    if len(failed) > 2:
        names.append(f"{len(failed) - 2} more")
    text = (", ".join(names[:-1]) + " and " + names[-1] if len(names) > 1 else names[0])
    text += " failed"
    if stuck["blocked"]:
        text += f', blocking {stuck["blocked"]} step{"s" if stuck["blocked"] != 1 else ""}'
    if stuck["paused"]:
        text += f' · {stuck["paused"]} paused'
    lead = "" if board.counts.get("running") else "Stopped: "
    icon = f'<span aria-hidden="true">{glyph("failed")}</span>' if mark else ""
    return f'<p class="stuck">{icon}<span>{lead}{text}</span></p>'


def failed_total(store: Store) -> int:
    """How many steps have failed across the active projects (the index's tab title)."""
    return sum(p["counts"].get("failed", 0) for p in store.projects() if not p["archived"])


def _title_mark(failed: int, since: list[str] | None = None) -> str:
    """What the tab title leads with (`2 failed · 1 quiet · `): how many steps failed, and
    when each running step last wrote, so static/sluice.js counts the quiet ones as they age
    and keeps the title current."""
    quiet = f' data-title-quiet="{e(" ".join(since))}"' if since else ""
    return f'<span hidden data-title-failed="{failed}"{quiet}></span>'


def title_lead(failed: int, since: Iterable[str]) -> str:
    """`2 failed · 1 quiet · `, or the part of it there is: a tab title's lead."""
    now = _now()
    quiet = sum(1 for t in since if (at := _parse_iso(t)) and (now - at).total_seconds() >= QUIET)
    return "".join(f"{n} {w} · " for n, w in ((failed, "failed"), (quiet, "quiet")) if n)


# ---- the project index ------------------------------------------------------------------


RUNNER_STALE = 15  # seconds without a beat before the runner counts as down


def _runner_beat(home: Path) -> tuple[str, str]:
    """The runner's liveness and its last beat. The runner heartbeats
    SLUICE_HOME/runner.json ({"pid", "started", "beat"}) about once a second and leaves it
    behind when it exits: ("live", beat) while the beat is fresh, ("stale", beat) past
    RUNNER_STALE seconds, ("none", "") when no runner has run in this home."""
    try:
        beat = str(read_json(home / "runner.json").get("beat") or "")
    except (OSError, ValueError, AttributeError):
        return "none", ""
    then = _parse_iso(beat)
    if then is None:
        return "none", ""
    if (_now() - then).total_seconds() <= RUNNER_STALE:
        return "live", beat
    return "stale", beat


def runner_state(home: Path) -> str:
    """The liveness alone ("live"/"stale"/"none"), for the streams' version: not the beat
    itself, or a page would re-render on every heartbeat."""
    return _runner_beat(home)[0]


def runner_note(store: Store) -> str:
    """The 'runner down' line of the index and a project page's summary, in the attention
    voice: only for a stale beat. No runner.json says nothing (a runner from before the
    heartbeat writes none, so its absence is not evidence)."""
    state, beat = _runner_beat(store.home)
    if state == "stale":
        return f'<p class="attn">Runner stopped · last seen {_when(beat)}</p>'
    return ""


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


def _bar(counts: Mapping[str, int], total: int,
         stuck: Mapping[str, int] | None = None) -> str:
    """Progress by status, proportional, with the same counts as text for assistive tech (the
    pending ones split into blocked, paused and the rest, from `stuck`)."""
    order = ("succeeded", "skipped", "running", "stale", "failed", "pending")
    said = ", ".join(f"{counts[s]} {s}" for s in order[:-1] if counts.get(s))
    held = {k: n for k, n in (stuck or {}).items() if n}
    rest = counts.get("pending", 0) - sum(held.values())
    said = ", ".join(x for x in (said, *(f"{n} {k}" for k, n in held.items()),
                                 f"{rest} pending" if rest else "") if x)
    segs = "".join(f'<i class="b-{s}" style="flex:{counts[s]}"></i>'
                   for s in order if counts.get(s))
    return f'<span class="bar" role="img" aria-label="{e(said or "no steps")}">{segs}</span>' \
        if total else '<span class="bar" role="img" aria-label="no steps"></span>'


def _project_row(store: Store, name: str, since: list[str] | None = None) -> str:
    """One project on the index (with `since`, it adds when each running step last wrote)."""
    info = store.project(name)
    href = f"/projects/{quote(name)}"
    about = f'<p class="about">{e(first_paragraph(info["description"]))}</p>' \
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
    stuck = attention(board, lambda sid: f"{href}#step:{quote(sid)}", mark=False)
    running = [b for b in board.blocks.values() if b.status == "running"]
    if since is not None:
        since += quiet_since(store, board)
    if running:
        now = "".join(
            f'<li><a href="{href}#step:{quote(b.sid)}">{glyph("running")}'
            f'<span class="ttl">{e(b.title)}</span></a><span class="dur">{_elapsed(b)}'
            f"{quiet_mark(store, name, b, bool(_elapsed(b)))}</span></li>" for b in running[:4])
        more = f'<li class="more">and {len(running) - 4} more</li>' if len(running) > 4 else ""
        now = f'<ul class="now">{now}{more}</ul>'
    elif info.get("paused") is True:
        now = '<p class="now">Paused.</p>'
    elif total and counts.get("succeeded", 0) + counts.get("skipped", 0) == total:
        now = '<p class="now">Finished.</p>'
    elif stuck:
        now = ""  # the attention line says it
    elif counts.get("failed") or counts.get("stale"):
        now = '<p class="now">Stopped: nothing is running.</p>'
    elif total:
        now = '<p class="now">Nothing is running.</p>'
    else:
        now = '<p class="now">No steps yet.</p>'
    done = f"{counts.get('succeeded', 0)} of {total}" if total else ""
    last = f'<span class="meta">{_when(when)}</span>' if when else ""
    return (f'<li class="proj"><div class="p-head"><a href="{href}">'
            f'{glyph(_project_status(counts), ", ")}<span>{e(name)}</span></a>{last}</div>'
            f'{stuck}{about}<div class="p-state">{_bar(counts, total, board.stuck)}'
            f'<span class="meta">{done}</span></div>{now}</li>')


def index_parts(store: Store) -> dict[str, str]:
    return _index_parts(store)[0]


def _index_parts(store: Store) -> tuple[dict[str, str], str]:
    """The index's parts, and its tab title's lead (`2 failed · 1 quiet · `)."""
    names = store.project_names()
    active = [n for n in names if not store.archived(n)]
    old = [n for n in names if n not in active]
    since: list[str] = []
    rows = "".join(_project_row(store, n, since) for n in active)
    body = f'<ul class="projects">{rows}</ul>' if rows else \
        ('<p class="empty">No projects yet. An orchestrator creates one with '
         "<code>project_create</code>.</p>" if not old else
         '<p class="empty">Every project is archived.</p>')
    if old:
        body += (f'<details class="archived" data-preserve-attr="open"><summary>Archived '
                 f'({len(old)})</summary><ul class="projects">'
                 f'{"".join(_project_row(store, n) for n in old)}</ul></details>')
    failed = failed_total(store)
    return ({"projects": _part("projects", runner_note(store) + body
                               + _title_mark(failed, since)),
             "nav-inbox": nav_inbox(open_count(store))}, title_lead(failed, since))


def index(store: Store, ver: str | None = None) -> str:
    """The project index; live (streaming from /stream) when given the home's version `ver`."""
    parts, lead = _index_parts(store)
    return layout(f"{lead}Projects",
                  f'<h1 class="vh">Projects</h1>'
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
    note = runner_note(store)
    note += '<p class="attn-note">Paused: no step starts until you resume it.</p>' \
        if paused else ""
    note += '<p class="attn-note">Archived: listed apart from the other projects.</p>' \
        if archived else ""
    counts, total = board.counts, len(board.blocks)
    stuck = attention(board, lambda sid: step_href(project, sid), drawer=live) if live else \
        attention(board, lambda sid: f"#step-{sid}")
    line = (f'{stuck}<div class="sumline">{_bar(counts, total, board.stuck)}<p class="meta sum">'
            f'{_summary_line(board, last_change(store, project))}</p>')
    if live:
        line += (f'<div class="switches">{_pause_form(project, paused)}'
                 f"{_archive_form(project, archived)}</div>")
    line += "</div>"
    # first whether the work is moving (and the switches), then what the project is; what the
    # plan took and produced sits under the board
    parts = {"summary": _part("summary", line + note + _about(about)
                              + _title_mark(len(board.failed), quiet_since(store, board))),
             # the skip link's target: focusable, so a keyboard lands on the plan
             "graph": f'<div id="graph" tabindex="-1">{board_html(store, board, live)}</div>',
             "result": _part("result", result_panel(board) + inputs_strip(board), "section",
                             "plan-facts")}
    if live:
        parts["nav-inbox"] = nav_inbox(open_count(store))
    return parts


def _drawer(project: str) -> str:
    """The step drawer, a <sluice-drawer>: `$step` (from the address's `#step:<id>`) opens it
    and streams that step's detail into it; the component (static/sluice.js) opens it from a
    step link, closes it (Escape, the close button, the scrim, a click on the page around the
    board) and gives focus back. A labelled region beside the page; below 1200px, over it, a
    modal dialog (the component sets its role and makes the page behind it inert). Then the polite live
    region the board announces status changes in."""
    url = f"'/projects/{quote(project)}/steps/' + encodeURIComponent($step) + '/stream'"
    effect = (f"$step ? @get({url}, {{retry: 'always', retryMaxCount: 1000000, "
              f"requestCancellation: window.sluiceStream ? window.sluiceStream() : 'auto'}}) "
              f": window.sluiceStream && window.sluiceStream()")
    hash_to_step = ("$step = location.hash.startsWith('#step:') ? "
                    "decodeURIComponent(location.hash.slice(6)) : ''")
    return (f'<sluice-drawer data-preserve-attr="data-rocket-host">'
            f'<div class="scrim" style="display:none" data-show="$step != \'\'" '
            f'data-on:click="window.sluiceClose && window.sluiceClose()"></div>'
            f'<aside id="drawer" class="drawer" style="display:none" tabindex="-1" '
            f'aria-labelledby="d-title" data-show="$step != \'\'" data-effect="{e(effect)}" '
            f'data-init="{e(hash_to_step)}" data-on:hashchange__window="{e(hash_to_step)}">'
            f'<div class="d-top"><button type="button" class="close" aria-label="Close" '
            f'data-on:click="window.sluiceClose && window.sluiceClose()">{X_ICON}</button></div>'
            f'<div id="step-detail"></div></aside></sluice-drawer>'
            f'<div id="announce" class="vh" role="status" aria-live="polite"></div>')


def project_page(store: Store, project: str, ver: str | None = None) -> str:
    """The project page: live (nav, links, the step drawer, streaming its changes) when given
    the project's version `ver`, else the standalone page plan_view returns (the board, then
    every step's detail in a disclosure)."""
    live = ver is not None
    p = _project(store, project, live)
    body = (f'{project_head(project, "plan" if live else None)}{p["summary"]}'
            f'{p["graph"]}{p["result"]}')
    board = load_board(store, project)
    if not live:
        body += "".join(
            f'<details class="std" id="step-{e(sid)}"><summary>{glyph(b.mark)}'
            f"<span>{e(b.title)}</span></summary>{step_detail(store, project, sid, False)}"
            f"</details>" for sid, b in board.blocks.items())
    lead = title_lead(len(board.failed), quiet_since(store, board))
    return layout(f"{lead}{project}", body, nav=live,
                  stream=f"/projects/{project}/stream" if live else None,
                  signals={"ver": ver, "step": "", "sver": ""} if live else None,
                  inbox=open_count(store) if live else None, here="/", board=live,
                  store=store, project=project, tab="plan",
                  skip=("graph", "Skip to plan") if live else None,
                  tail=_drawer(project) if live else "")


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


def _runs(recs: list[dict[str, Any]], sid: str) -> list[dict[str, Any]]:
    """The step's attempts from its step.status records: started, finished, outcome."""
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
    run (status, fn, what it waits on, started, duration, cost, session; a link to its thread),
    then what matters now (error, progress), what it produced, its prompt and other inputs (where each comes
    from), its log output and, when it ran more than once, its attempts. Types show on demand
    (the Types switch; always in a name's title). The `step-detail` part of the drawer and of
    the step page."""
    board = load_board(store, project)
    b = board.blocks.get(sid)
    if b is None:
        raise NotFound(f"the plan of project {project} has no step {sid!r}")
    outs_all = b.entry.get("outputs") if isinstance(b.entry.get("outputs"), dict) else {}

    def steps(ids: Iterable[str]) -> str:
        """Steps as links (the drawer's, live), each led by its status glyph."""
        return ", ".join(
            f'<span class="dep">{glyph(board.blocks[d].mark, ", ")}'
            + (f'<a href="{e(step_href(project, d))}" data-step="{e(d)}">{e(d)}</a>' if live
               else e(d)) + "</span>" for d in ids if d in board.blocks)

    status = "blocked" if board.blocked(sid) else WORDS.get(b.mark, b.status)
    facts = [("Status", e(status)), ("Function", f"<code>{e(b.fn)}</code>")]
    wide: list[tuple[str, str]] = []  # facts that take a row of their own and wrap
    if b.when is not None:
        ref = e(str(b.when))
        if b.when.step in board.blocks:
            ref = steps([b.when.step]).replace(f">{e(b.when.step)}</", f">{ref}</", 1)
        wide.append(("When", ref))
    if b.after:
        wide.append(("After", steps(b.after)))
    if b.status == "failed" and (held := board.blocks_of(sid)):
        wide.append(("Blocks", steps(held)))
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
    if b.status == "skipped":
        doc += f'<p class="d-doc">Skipped: {e(b.entry.get("skipped") or "")}</p>'
    if b.paused and b.status != "running":
        doc += f'<p class="d-doc attn-note">Paused{": " + e(b.pause_reason) if b.pause_reason else ""}</p>'
    waits = waits_on(board, b)
    if waits:
        wide.insert(0, ("Waits on", steps(d for d, _ in waits)))
    grid = "".join(f"<div><dt>{k}</dt><dd>{v}</dd></div>" for k, v in facts)
    grid += "".join(f'<div class="wide"><dt>{k}</dt><dd>{v}</dd></div>' for k, v in wide)
    thread = f"step-{sid}"  # its conversation with the orchestrator, on the Threads tab
    recs = L.read(store.log_dir(project), threads=[thread],
                  kinds=["step.status", "step.output", "message"])["records"]
    msgs = [r for r in recs if r.get("kind") == "message"]
    talk = ""
    if msgs and live:
        waiting = len(_awaiting(msgs, board.blocks))
        n = f'{len(msgs)} message{"s" if len(msgs) != 1 else ""}'
        n += f' · <span class="attn">{waiting} awaiting reply</span>' if waiting else ""
        talk = (f'<a class="d-thread" href="{e(threads_href(project, thread))}">Thread · {n}'
                f"</a>")
    # pausing holds a step that has not started; it never stops a running one (SPEC §6), and
    # a finished one would not run again anyway: the switch shows only where it acts
    pause = _pause_form(project, b.paused, sid) \
        if b.paused or b.status in ("pending", "failed", "stale") else ""
    switch = f'<div class="d-actions">{pause}{talk}</div>' if live and (pause or talk) else ""
    hid = ' id="d-title"' if live else ""
    head = (f'<header class="d-head"><div class="hd">{glyph(b.mark)}<h2{hid}>{e(sid)}</h2>'
            f'</div>{doc}<dl class="facts">{grid}</dl>{switch}</header>')
    sections = []
    # one Types switch, on the first section of values
    types = [('<button type="button" class="types-toggle" aria-pressed="false" '
              'title="Show the types of the values">Types</button>')]

    def section(title: str, body: str, extra: str = "") -> None:
        sections.append(f'<section class="d-sec"><div class="d-sec-h">{_label(title)}{extra}'
                        f"</div>{body}</section>")

    def types_switch() -> str:
        return types.pop() if types else ""

    if b.entry.get("error"):
        err = str(b.entry["error"]).strip()
        headline = error_headline(err)
        # all of it as it was raised, scrolled to its end (the cause is the last line)
        full = (f'<div class="err-box"><pre class="err">{e(err)}</pre></div>'
                if err != headline else "")
        section("Error", f'<p class="err-line">{e(headline)}</p>{full}')
    tail = ""
    if b.run_ids:
        d = _run_dir(store, project, b.run_ids[-1])
        tail = tail_text(d / "stderr.log", TAIL).strip() if d else ""
    which = f" (run {len(b.run_ids)} of {int(b.entry['total'])})" \
        if "total" in b.entry and len(b.run_ids) > 1 else ""
    if b.status == "running":
        line, quiet = "", False
        if (act := _activity(store, project, b)) is not None:
            quiet = act[1] >= QUIET
            last = progress_line(store, project, b)
            # the ticker (static/sluice.js) fills the line in and unhides it when the run
            # crosses QUIET without another write
            line = (f'<p class="attn" data-quiet-line="{e(act[0])}"'
                    f'{"" if quiet else " hidden"}><span class="q">'
                    f'{e(f"Quiet for {dur(act[1])}.") if quiet else ""}</span> '
                    f'{e("Last output: " + last if last else "No output yet.")}</p>')
        line += (f'<pre class="tail">{e(tail)}</pre>' if tail else
                 "" if quiet else '<p class="quiet">Nothing written yet.</p>')
        section("Progress" + which, line)
    # outputs: what it produced (its declared ones first); session and cost are run facts
    outs: dict[str, Any] | None = outs_all if isinstance(b.entry.get("outputs"), dict) else None
    declared = b.outputs
    title = "Outputs"
    own = {n: t for n, t in declared.items() if n in b.submitted}
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
            section(title, f'<div class="fields">{"".join(fields)}</div>', types_switch())
    elif own:
        names = ", ".join(e(n) for n in own)
        section("Outputs", f'<p class="quiet">None yet. It hands on: {names}.</p>')
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
        src = b.step.sources.get(name)
        if src is None:
            return False, None
        if not src.refs:
            return True, src.default
        ok = all(value_of(r, board.plan, board.state)[0] for r in src.refs)
        return ok, source_value(src, board.plan, board.state)

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
        section("Inputs", f'<div class="fields">{"".join(fields)}</div>', types_switch())
    if tail and b.status != "running":
        lines = tail.splitlines()
        body = f'<pre class="tail">{e(tail)}</pre>'
        if len(lines) > FOLD_LINES:
            body = (f'<details data-preserve-attr="open"><summary>Show {len(lines)} lines'
                    f"</summary>{body}</details>")
        section("Log output" + which, body)
    runs = _runs(recs, sid)
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


# what a restarted runner found of a leftover run (`run.adopt`'s outcome, SPEC §6)
ADOPTED = {"watching": "still running; the new runner watches it",
           "finished": "had finished; its result was collected",
           "unknown": "had stopped without saying how it ended",
           "restarted": "was lost in the restart"}


def log_summary(rec: dict[str, Any]) -> str:
    """One line (HTML) saying what a log record is about."""
    kind = rec.get("kind")
    by = f"rev {rec.get('rev')} by {rec.get('author') or '?'}"
    if rec.get("reason"):
        by += f": {_line(rec['reason'], 80)}"
    if kind == "step.status":
        text = e(f"{rec.get('step')} {rec.get('from') or 'new'} → ") + _st(rec.get("to"))
        return text + (e(": " + error_headline(rec["error"], 120)) if rec.get("error") else "")
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
    if kind == "step.cancel":
        who = f" by {rec['author']}" if rec.get("author") else ""
        why = f": {_line(rec['reason'], 80)}" if rec.get("reason") else ""
        return e(f"{rec.get('step')} cancelled{who}{why}")
    if kind == "call":
        text = e(f"{rec.get('call')} {rec.get('fn')} ") + _st(rec.get("status"))
        return text + (e(": " + _line(rec["error"])) if rec.get("error") else "")
    if kind == "message":
        to = f" → {rec['to']}" if rec.get("to") else ""
        # a step's own thread (`step-<id>`) goes without saying
        who = str(rec.get("from")) if rec.get("thread") == f"step-{rec.get('from')}" \
            else f"{rec.get('thread')} from {rec.get('from')}"
        return e(f"{who}{to}: {_line(rec.get('body', ''))}")
    if kind == "run.adopt":
        who = rec.get("step") or f"call {rec.get('call')}"
        how = ADOPTED.get(str(rec.get("outcome")), f"adopted ({rec.get('outcome')})")
        return e(f"{who}: run {rec.get('run')} {how}")
    if kind == "run.orphan":
        return e(f"run {rec.get('run')} stopped: no step or call claimed it")
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


def log_shown(rec: dict[str, Any], q: LogQuery) -> bool:
    """Whether the Log viewer lists a record: a `thread.post` call's `call` rows are noise
    beside its `message` row, so they hide unless the kinds filter names `call` — a failed
    one always shows. Only the viewer does this; the tools list everything."""
    return not (rec.get("kind") == "call" and rec.get("fn") == "thread.post"
                and rec.get("status") != "failed" and "call" not in q.kinds)


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
    recs = [r for r in res["records"] if log_shown(r, q)]
    base = log_base(project)

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

    def box(k: str, text: str, cls: str = "") -> str:
        c = f' class="{cls}"' if cls else ""
        return (f'<label{c}><input type="checkbox" name="kind" value="{k}" data-bind:kinds'
                f'{" checked" if k in q.kinds else ""}> {e(text)}</label>')

    # one line per group — its checkbox then its kinds by their short names — and one for
    # the ungrouped kinds; the order is still KIND_OPTIONS, so data-bind:kinds lines up
    # with the `kinds` signal
    lines: list[tuple[str, list[str]]] = []
    for k in KIND_OPTIONS:
        g = k if k in L.GROUPS else (k.split(".", 1)[0] if k.split(".", 1)[0] in L.GROUPS
                                     else "")
        text = k[len(g) + 1:] if g and k != g else k
        if not lines or lines[-1][0] != g:
            lines.append((g, []))
        lines[-1][1].append(box(k, text, "kg" if k == g else ""))
    boxes = "".join(f'<span class="kline">{"".join(bs)}</span>' for _, bs in lines)
    # on a phone the kinds fold behind a summary (static/sluice.js closes it there and keeps
    # its count current), so the records start near the top; open without script
    n = len(q.kinds)
    said = f"Filter: {n} kind{'s' if n != 1 else ''}" if n else "Filter: all kinds"
    form = (f'<form class="filters" method="get" action="{e(base)}" '
            f'data-on:input__debounce.300ms="{e(apply)}" data-on:submit="{e(apply)}">'
            f'<details class="kinds" open><summary><span>{said}</span>{CHEVRON}</summary>'
            f"<fieldset><legend>Kinds</legend>{boxes}</fieldset></details>"
            f'<label class="thread">Threads <input name="thread" '
            f'value="{e(",".join(q.threads))}" placeholder="any" size="16" data-bind:thread>'
            f"</label><button>Apply</button></form>")
    if project is None:
        title = '<h1 class="vh">Log</h1><p class="meta">Calls made without a project.</p>'
        name = "Log"
    else:
        history = bool(q.kinds) and set(q.kinds) == set(L.HISTORY_KINDS) and not q.threads
        tab = "history" if history else "log"
        title = project_head(project, tab)
        name = f'{"History" if history else "Log"} · {project}'  # as the Threads tab's
    signals = {"kinds": [k if k in q.kinds else "" for k in KIND_OPTIONS],
               "thread": ",".join(q.threads), "before": q.before or 0, "after": q.after or 0,
               "view": q.query(), "seen": last}
    url = "history.replaceState(null, '', location.pathname + ($view ? '?' + $view : ''))"
    return layout(name, f"{title}{form}{view}",
                  stream=f"{base}/stream", signals=signals,
                  main_attrs=f' data-effect="{e(url)}"', inbox=open_count(store),
                  here="/log" if project is None else "/", board=True,
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
    names: dict[str, list[str]] = {}
    for x in reg.listing():
        names.setdefault(x["scope"], []).append(x["name"])
        cls = "fn problem" if x.get("error") else "fn"
        err = f'<p class="err">{e(x["error"])}</p>' if x.get("error") else ""
        groups.setdefault(x["scope"], []).append(
            f'<div class="{cls}" id="fn-{e(x["name"])}"><div class="fn-head"><b>{e(x["name"])}</b> '
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
        index = "".join(f'<a href="#fn-{e(n)}">{e(n)}</a>' for n in names.get(scope, []))
        index = f'<nav class="fn-index" aria-label="{e(SCOPE_TITLES[scope])}">{index}</nav>' \
            if len(names.get(scope, [])) > 3 else ""
        sections.append(f"<h2>{title}</h2>{index}{cards}")
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


def inbox_parts(store: Store, project: str | None, status: str) -> dict[str, str]:
    """The inbox page's parts: its items (open ones oldest first, the rest newest first), on
    the open view the plan inputs waiting on a person, and the nav badge."""
    items = store.inbox(project, status)
    items = items if status == "open" else items[::-1]
    back = inbox_base(project) + ("" if status == "open" else f"?status={status}")
    empty = {"open": "Nothing is waiting on you."}.get(status, f"No {status} items.")
    body = "".join(_item(i, back, project is None) for i in items) \
        or f'<p class="empty">{e(empty)}</p>'
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
