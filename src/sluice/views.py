"""Views (SPEC §8): the HTML of the read-only dashboard `sluice serve` renders, and `plan_view`.

- `mermaid(plan, state)`: the plan as a flowchart with one colour per step status.
- `project_page`: summary, diagram, plan inputs and outputs, steps (each expands to its inputs,
  outputs and stderr tail), the recent plan history and the latest log records. `render()`
  serves it (standalone) to `plan_view` too.
- `index`: every project; `fns_page`: every visible function grouped by scope; `log_page`: one
  page of a log, filtered (`LogQuery`).

A live page (given its stream URL) loads Datastar and opens one SSE stream; `dashboard` sends
the parts that changed, re-rendered by the same `*_parts` functions, as element patches. Each
part is one element with an id. Everything here only reads the store, and every value is
HTML-escaped (plans and logs are untrusted).
"""

from __future__ import annotations

import dataclasses
import datetime as dt
import html
import json
from collections.abc import Iterable, Mapping
from typing import Any
from urllib.parse import urlencode

from . import log as L
from .errors import BadRequest
from .plan import Plan, value_of
from .store import Store
from .util import read_json, tail_text

CLASSES = {"pending": "fill:#f1f1f1,stroke:#999,color:#333",
           "running": "fill:#dbeafe,stroke:#2563eb,color:#1e3a8a",
           "succeeded": "fill:#dcfce7,stroke:#16a34a,color:#14532d",
           "failed": "fill:#fee2e2,stroke:#dc2626,color:#7f1d1d",
           "stale": "fill:#fef3c7,stroke:#d97706,color:#78350f",
           "manual": "fill:#fff,stroke:#16a34a,stroke-width:3px,stroke-dasharray:6 3"}
STATUSES = ("pending", "running", "succeeded", "stale", "failed")
MERMAID_JS = "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs"
DATASTAR_JS = "https://cdn.jsdelivr.net/gh/starfederation/datastar@v1.0.4/bundles/datastar.js"
# Keep the stream open across server restarts and network blips (Datastar backs off to 30 s).
STREAM_OPTIONS = "{retry: 'always', retryMaxCount: 1000000}"
HISTORY = 20
RECENT = 10  # log records on the project page
PAGE_SIZE = 50  # log records per log page
SCOPE_TITLES = {"builtin": "Built-in", "global": "Global", "project": "Project"}
# The log viewer's kind filter: each group name, then the kinds under it (§6b).
KIND_OPTIONS = tuple(dict.fromkeys(
    x for k in L.KINDS for x in [*(g for g in L.GROUPS if k.startswith(g + ".")), k]))

e = html.escape


# ---- Mermaid ----------------------------------------------------------------------------


def _q(text: str) -> str:
    return '"' + text.replace('"', "#quot;") + '"'


def step_label(sid: str, run: str, entry: dict[str, Any]) -> str:
    return f"{sid} / {run} / {_status(entry)}"


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
        lines.append(f"  {ids['step', sid]}[{_q(step_label(sid, step.fn.name, entry))}]")
    for n in plan.outputs:
        lines.append(f"  {ids['out', n]}([{_q(n)}])")

    def edge(ref, target: str) -> str:
        src = ids["step", ref.step] if ref.step else ids["in", ref.name]
        return f"  {src} -->|{_q(ref.name)}| {target}"

    for sid, step in plan.steps.items():
        lines.extend(dict.fromkeys(edge(r, ids["step", sid]) for r in step.reads))
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


# ---- layout -----------------------------------------------------------------------------

CSS = """
:root{--bg:#fff;--fg:#1f2328;--muted:#656d76;--line:#d0d7de;--card:#f6f8fa;--link:#0969da;
--bad:#cf222e;--badbg:#ffebe9;--ok:#1a7f37;--run:#0969da;--stale:#9a6700;--code:#eff1f3}
@media (prefers-color-scheme: dark){:root{--bg:#0d1117;--fg:#e6edf3;--muted:#8d96a0;
--line:#30363d;--card:#161b22;--link:#4493f8;--bad:#f85149;--badbg:#3b1219;--ok:#3fb950;
--run:#4493f8;--stale:#d29922;--code:#1f242c}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,sans-serif}
body>nav{display:flex;flex-wrap:wrap;gap:.4rem 1rem;align-items:center;padding:.6rem 1rem;
border-bottom:1px solid var(--line)}
body>nav b{margin-right:.5rem}
a{color:var(--link);text-decoration:none}a:hover{text-decoration:underline}
main{padding:1rem;max-width:1200px;margin:0 auto}
h1{font-size:1.4rem;margin:.2rem 0}h2{font-size:1.1rem;margin:1.4rem 0 .5rem}
h2 small,h1 small{font-weight:normal;font-size:.8em}
.muted{color:var(--muted)}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%}
td,th{border-bottom:1px solid var(--line);padding:.35rem .5rem;text-align:left;vertical-align:top}
th{font-weight:600;color:var(--muted)}
td.num{text-align:right;font-variant-numeric:tabular-nums}
.nowrap{white-space:nowrap}
code,pre{background:var(--code);border-radius:4px;font-size:.88em}
code{padding:0 .25em}pre{padding:.5rem;overflow-x:auto;white-space:pre-wrap;margin:.3rem 0}
.diagram{background:var(--card);border-radius:6px;text-align:center;overflow-x:auto;
padding:.5rem;margin:.5rem 0}
.diagram pre{background:none;text-align:left}
.card{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:.6rem .8rem;
margin:.5rem 0}
.s-failed,.bad{color:var(--bad)}.s-succeeded{color:var(--ok)}.s-running{color:var(--run)}
.s-stale{color:var(--stale)}
.problem{background:var(--badbg);border-color:var(--bad)}
details summary{cursor:pointer}
td details summary{overflow-wrap:anywhere}
form.filters{display:flex;flex-wrap:wrap;gap:.4rem 1rem;align-items:center;margin:.5rem 0}
form.filters fieldset{border:0;padding:0;margin:0;display:flex;flex-wrap:wrap;gap:.2rem .8rem}
form.filters legend{float:left;margin-right:.6rem;color:var(--muted)}
form.filters label{white-space:nowrap}
input,button{font:inherit;color:inherit;background:var(--bg);border:1px solid var(--line);
border-radius:4px;padding:.15rem .4rem}
input[type=checkbox]{padding:0}
button{background:var(--card);cursor:pointer}
.pager{display:flex;gap:1rem;margin:.6rem 0}
.log-view:has(tbody tr) .log-empty,.log-view:not(:has(tbody tr)) .scroll{display:none}
table.log td:last-child{min-width:14rem}
@media (max-width:600px){table.log .at{display:none}}
"""

DIAGRAM_JS = """
import mermaid from "MERMAID_JS";
mermaid.initialize({startOnLoad: false,
  theme: matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "default"});
let n = 0;
// Render the source element's Mermaid text into its view; a newer render wins.
window.sluiceDiagram = async (src) => {
  const view = document.getElementById(src.dataset.view), id = ++n;
  try {
    const {svg} = await mermaid.render("mermaid-" + id, src.textContent);
    if (id === n && view) view.innerHTML = svg;
  } catch (err) { console.error(err); }
};
const src = document.getElementById("plan-src");
if (src) window.sluiceDiagram(src); else mermaid.run({querySelector: "pre.mermaid"});
""".replace("MERMAID_JS", MERMAID_JS)


def _signals(values: Mapping[str, Any]) -> str:
    return e(json.dumps(values, ensure_ascii=False))


def layout(title: str, body: str, diagram: bool = False, nav: bool = True,
           stream: str | None = None, signals: Mapping[str, Any] | None = None,
           main_attrs: str = "") -> str:
    """A page. With `stream`, Datastar opens that SSE stream once the page has loaded (with
    `signals`, the page's Datastar signals, sent along as the `datastar` query parameter)."""
    head = f'<script type="module" src="{DATASTAR_JS}"></script>' if stream else ""
    script = f'<script type="module">{DIAGRAM_JS}</script>' if diagram else ""
    top = ('<nav><b>sluice</b><a href="/">Projects</a> · <a href="/fns">Functions</a> · '
           '<a href="/log">Log</a></nav>' if nav else "")
    body_attrs = f' data-signals="{_signals(signals)}"' if signals else ""
    if stream:
        main_attrs += f' data-init="@get(\'{e(stream)}\', {STREAM_OPTIONS})"'
    return (f'<!doctype html>\n<html><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width,initial-scale=1">'
            f"<title>sluice: {e(title)}</title><style>{CSS}</style>{head}</head>\n"
            f"<body{body_attrs}>{top}<main{main_attrs}>\n{body}\n</main>{script}</body></html>\n")


def _json(value: Any) -> str:
    return e(json.dumps(value, indent=2, ensure_ascii=False))


def _values_table(values: dict[str, Any]) -> str:
    rows = "".join(f"<tr><td>{e(k)}</td><td><code>{e(json.dumps(v, ensure_ascii=False))}"
                   f"</code></td></tr>" for k, v in values.items())
    return (f'<div class="scroll"><table><tr><th>name</th><th>value</th></tr>{rows}</table>'
            f"</div>" if rows else '<p class="muted">none</p>')


def _counts(counts: dict[str, int]) -> str:
    parts = [f'<span class="s-{s}">{counts[s]} {s}</span>' for s in STATUSES if counts.get(s)]
    return ", ".join(parts) or '<span class="muted">no steps</span>'


def _part(pid: str, inner: str, tag: str = "div") -> str:
    return f'<{tag} id="{pid}">{inner}</{tag}>'


def not_found(message: str) -> str:
    return layout("not found", f"<p>{e(message)}</p>")


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


def index_parts(store: Store) -> dict[str, str]:
    rows = []
    for p in store.projects():
        rows.append(f'<tr><td><a href="/projects/{e(p["name"])}">{e(p["name"])}</a></td>'
                    f'<td>{e(p["description"])}</td><td>{_counts(p["counts"])}</td>'
                    f'<td>rev {p["rev"]}</td><td class="muted">{e(last_change(store, p["name"]))}'
                    f"</td></tr>")
    table = ('<div class="scroll"><table><tr><th>project</th><th>description</th><th>steps</th>'
             '<th>plan</th><th>last change</th></tr>' + "".join(rows) + "</table></div>"
             if rows else '<p class="muted">No projects yet.</p>')
    return {"projects": _part("projects", table)}


def index(store: Store, ver: str | None = None) -> str:
    """The project index; live (streaming from /stream) when given the home's version `ver`."""
    parts = index_parts(store)
    return layout("projects", f"<h1>Projects</h1>{parts['projects']}",
                  stream="/stream" if ver else None, signals={"ver": ver} if ver else None)


# ---- the project page -------------------------------------------------------------------


def _run_details(store: Store, project: str, entry: dict[str, Any]) -> tuple[list, str]:
    """The inputs of each run of a step (from its run dirs) and the last run's stderr tail."""
    inputs, tail = [], ""
    for run_id in entry.get("run_ids") or []:
        run_dir = store.runs_dir(project) / run_id
        try:
            inputs.append(read_json(run_dir / "input.json"))
        except (OSError, ValueError):
            inputs.append(None)
        tail = tail_text(run_dir / "stderr.log", 1500).strip() or tail
    return inputs, tail


def _steps(store: Store, project: str, doc: dict[str, Any], plan: Plan,
           state: dict[str, Any]) -> str:
    rows = []
    for sid, step in plan.steps.items():
        entry = state["steps"].get(sid, {"status": "pending"})
        err = (entry.get("error") or "").splitlines()[:1]
        status = _status(entry) + (" (manual)" if entry.get("manual") else "")
        inputs, tail = _run_details(store, project, entry)
        parts = [f"<div>bindings</div><pre>{_json(doc['steps'][sid].get('in', {}))}</pre>"]
        if inputs:
            shown = inputs[0] if len(inputs) == 1 else inputs
            parts.append(f"<div>inputs</div><pre>{_json(shown)}</pre>")
        if entry.get("outputs") is not None:
            parts.append(f"<div>outputs</div><pre>{_json(entry['outputs'])}</pre>")
        if entry.get("error"):
            parts.append(f'<div>error</div><pre class="bad">{e(entry["error"])}</pre>')
        if tail:
            parts.append(f"<div>stderr (tail)</div><pre>{e(tail)}</pre>")
        rows.append(
            f'<tr><td><details id="step-{e(sid)}" data-preserve-attr="open"><summary>{e(sid)}'
            f'</summary>{"".join(parts)}</details></td><td>{e(step.fn.name)}</td>'
            f'<td class="s-{e(entry["status"])}">{e(status)}</td>'
            f'<td>{e(entry.get("started") or "")}</td><td>{e(entry.get("finished") or "")}</td>'
            f'<td class="bad">{e(err[0]) if err else ""}</td></tr>')
    return ('<div class="scroll"><table><tr><th>step</th><th>fn</th><th>status</th>'
            "<th>started</th><th>finished</th><th>error</th></tr>" + "".join(rows)
            + "</table></div>" if rows else '<p class="muted">The plan has no steps.</p>')


def _history(store: Store, project: str) -> str:
    rows = []
    for x in reversed(store.history(project)[-HISTORY:]):
        what = x["kind"] + (f" ({len(x.get('ops') or [])} ops)" if "ops" in x else "")
        what += f" {x['step']}" if "step" in x else f" {x['name']}" if "name" in x else ""
        what += " (forced)" if x.get("force") else ""
        rows.append(f"<tr><td>{e(str(x['rev']))}</td><td>{e(x['at'])}</td>"
                    f"<td>{e(x['author'])}</td><td>{e(what)}</td>"
                    f"<td>{e(x.get('reason') or '')}</td></tr>")
    return ('<div class="scroll"><table><tr><th>rev</th><th>at</th><th>author</th><th>what</th>'
            f'<th>reason</th></tr>{"".join(rows)}</table></div>')


def project_parts(store: Store, project: str) -> dict[str, str]:
    """The live project page's parts by element id: what its stream patches. `plan-src` holds
    the diagram's Mermaid text, which the page renders into `plan-diagram`."""
    return _project(store, project, True)[0]


def _project(store: Store, project: str, live: bool) -> tuple[dict[str, str], str]:
    """The project page's parts and its Mermaid text; without `live`, the parts of the
    standalone page (no links, no log)."""
    info = store.project(project)
    doc, plan = store.plan(project)
    state = store.read_state(project)
    counts: dict[str, int] = {}
    for sid in plan.steps:
        status = state["steps"].get(sid, {"status": "pending"})["status"]
        counts[status] = counts.get(status, 0) + 1
    about = info.get("description") or ""
    links = (f' · <a href="/projects/{e(project)}/log">log</a> · '
             f'<a href="/fns?project={e(project)}">functions</a>' if live else "")
    summary = (f'<h1>{e(project)} <small class="muted">rev {e(str(doc["rev"]))}</small></h1>'
               + (f"<p>{e(about)}</p>" if about else "") + f"<p>{_counts(counts)}{links}</p>")
    inputs = {n: state["inputs"].get(n) for n in plan.inputs}
    outputs = {n: value_of(r, plan, state)[1] for n, r in plan.outputs.items()}
    source = e(mermaid(plan, state))
    parts = {"summary": _part("summary", summary)}
    if live:
        parts["plan-src"] = (f'<pre id="plan-src" hidden data-view="plan-diagram" '
                             f'data-init="window.sluiceDiagram?.(el)">\n{source}</pre>')
    parts.update({
        "inputs": _part("inputs", _values_table(inputs)),
        "outputs": _part("outputs", _values_table(outputs)),
        "steps": _part("steps", _steps(store, project, doc, plan, state)),
        "history": _part("history", _history(store, project)),
    })
    if live:
        recent = L.page(store.log_dir(project), size=RECENT)["records"]
        parts["recent"] = _part("recent", _log_table(recent))
    return parts, source


def project_page(store: Store, project: str, ver: str | None = None) -> str:
    """The project page: live (nav, links, log, streaming its changes) when given the
    project's version `ver`, else the standalone page plan_view returns."""
    live = ver is not None
    p, source = _project(store, project, live)
    body = f"""{p["summary"]}
{p.get("plan-src", "")}<div id="plan-diagram" class="diagram"><pre class="mermaid">
{source}</pre></div>
<h2>Inputs</h2>{p["inputs"]}
<h2>Outputs</h2>{p["outputs"]}
<h2>Steps</h2>{p["steps"]}
<h2>History</h2>{p["history"]}"""
    if live:
        body += (f'\n<h2>Log <small><a href="/projects/{e(project)}/log">all records</a></small>'
                 f'</h2>{p["recent"]}')
    return layout(project, body, diagram=True, nav=live,
                  stream=f"/projects/{project}/stream" if live else None,
                  signals={"ver": ver} if live else None)


def render(store: Store, project: str, fmt: str) -> str:
    """plan_view: the Mermaid text or the standalone HTML page."""
    if fmt == "mermaid":
        plan = store.plan(project)[1]
        return mermaid(plan, store.read_state(project))
    if fmt == "html":
        return project_page(store, project)
    raise BadRequest(f'format must be "mermaid" or "html", got {fmt!r}')


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


def _line(text: Any, width: int = 120) -> str:
    """The first line of `text`, at most `width` characters, with … when anything is cut."""
    lines = str(text).strip().splitlines() or [""]
    line, more = lines[0], len(lines) > 1
    if len(line) > width:
        line, more = line[:width - 1], True
    return line + ("…" if more else "")


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
    if kind == "step.retry":
        return e(f"{rec.get('step')} retried · {by}")
    if kind == "call":
        text = e(f"{rec.get('call')} {rec.get('fn')} ") + _st(rec.get("status"))
        return text + (e(": " + _line(rec["error"])) if rec.get("error") else "")
    if kind == "message":
        to = f" → {rec['to']}" if rec.get("to") else ""
        return e(f"{rec.get('thread')} from {rec.get('from')}{to}: {_line(rec.get('body', ''))}")
    return e(_line(json.dumps(rec, ensure_ascii=False)))


def log_row(rec: dict[str, Any]) -> str:
    return (f'<tr id="r{int(rec["seq"])}"><td class="num">{int(rec["seq"])}</td>'
            f'<td class="muted nowrap at">{e(str(rec.get("at", "")))}</td>'
            f'<td><code>{e(str(rec.get("kind", "")))}</code></td>'
            f'<td><details data-preserve-attr="open"><summary>{log_summary(rec)}</summary>'
            f"<pre>{_json(rec)}</pre></details></td></tr>")


def log_rows(records: Iterable[dict[str, Any]]) -> str:
    return "".join(map(log_row, records))


def _log_table(records: list[dict[str, Any]], body_id: str | None = None) -> str:
    """Records (newest first) as a table; an empty one says so."""
    if not records and body_id is None:
        return '<p class="muted">No records yet.</p>'
    tbody = f'<tbody id="{body_id}">' if body_id else "<tbody>"
    return ('<div class="scroll"><table class="log"><thead><tr><th>seq</th><th class="at">at'
            f"</th><th>kind</th><th>what</th></tr></thead>{tbody}{log_rows(records)}</tbody></table></div>")


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
    body = ('<p class="muted log-empty">No matching records.</p>'
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
            f"<fieldset><legend>kinds</legend>{boxes}</fieldset>"
            f'<label>threads <input name="thread" value="{e(",".join(q.threads))}" '
            f'placeholder="any" size="16" data-bind:thread></label>'
            f"<button>Apply</button></form>")
    title = (f'Log <small class="muted">of <a href="/projects/{e(project)}">{e(project)}</a>'
             f"</small>" if project else
             'Log <small class="muted">of calls without a project</small>')
    signals = {"kinds": [k if k in q.kinds else "" for k in KIND_OPTIONS],
               "thread": ",".join(q.threads), "before": q.before or 0, "after": q.after or 0,
               "view": q.query(), "seen": last}
    url = "history.replaceState(null, '', location.pathname + ($view ? '?' + $view : ''))"
    return layout(f"{project or 'home'} log", f"<h1>{title}</h1>{form}{view}",
                  stream=f"{base}/stream", signals=signals,
                  main_attrs=f' data-effect="{e(url)}"')


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
        return '<span class="muted">none</span>'
    return "<br>".join(f"{e(k)}: <code>{e(type_text(v))}</code>" for k, v in ports.items())


def fns_page(store: Store, project: str | None = None) -> str:
    reg = store.registry(project)
    groups: dict[str, list[str]] = {}
    for x in reg.listing():
        cls = "card problem" if x.get("error") else "card"
        err = f'<div class="bad">{e(x["error"])}</div>' if x.get("error") else ""
        groups.setdefault(x["scope"], []).append(
            f'<div class="{cls}"><div><b>{e(x["name"])}</b> '
            f'<span class="muted">{e(x.get("doc") or "")}</span></div>{err}'
            f'<div class="scroll"><table><tr><th>inputs</th><th>outputs</th></tr><tr>'
            f"<td>{_ports(x.get('inputs'))}</td><td>{_ports(x.get('outputs'))}</td></tr>"
            f"</table></div></div>")
    other = [p for p in reg.problems if not p["where"].endswith("fn.json")]  # e.g. a missing dir
    options = "".join(f'<option value="{e(n)}"{" selected" if n == project else ""}>{e(n)}'
                      f"</option>" for n in store.project_names())
    picker = (f'<form method="get" action="/fns">as seen by <select name="project" '
              f'onchange="this.form.submit()"><option value="">(no project)</option>{options}'
              f"</select></form>")
    sections = []
    for scope in ("builtin", "global", "project"):
        if scope == "project" and project is None:
            continue
        title = SCOPE_TITLES[scope] + (f" ({e(project)})" if scope == "project" else "")
        cards = "".join(groups.get(scope, [])) or '<p class="muted">none</p>'
        sections.append(f"<h2>{title}</h2>{cards}")
    extra = "".join(f'<div class="card problem bad">{e(p["where"])}: {e(p["message"])}</div>'
                    for p in other)
    return layout("functions", f"<h1>Functions</h1>{picker}{extra}{''.join(sections)}")
