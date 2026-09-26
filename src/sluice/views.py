"""Views (SPEC §8): the read-only dashboard `sluice serve` renders, and `plan_view`.

- `mermaid(plan, state)`: the plan as a flowchart with one colour per step status.
- `project_page`: diagram, plan inputs and outputs, steps (each expands to its inputs, outputs
  and stderr tail) and the recent plan history. `render()` serves it to `plan_view` too.
- `index`: every project; `fns_page`: every visible function grouped by scope.

Everything here only reads the store, and every value is HTML-escaped (plans are untrusted).
"""

from __future__ import annotations

import datetime as dt
import html
import json
from typing import Any

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
HISTORY = 20
SCOPE_TITLES = {"builtin": "Built-in", "global": "Global", "project": "Project"}

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
nav{display:flex;gap:1rem;align-items:center;padding:.6rem 1rem;border-bottom:1px solid var(--line)}
nav b{margin-right:.5rem}
a{color:var(--link);text-decoration:none}a:hover{text-decoration:underline}
main{padding:1rem;max-width:1200px;margin:0 auto}
h1{font-size:1.4rem;margin:.2rem 0}h2{font-size:1.1rem;margin:1.4rem 0 .5rem}
.muted{color:var(--muted)}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%}
td,th{border-bottom:1px solid var(--line);padding:.35rem .5rem;text-align:left;vertical-align:top}
th{font-weight:600;color:var(--muted)}
code,pre{background:var(--code);border-radius:4px;font-size:.88em}
code{padding:0 .25em}pre{padding:.5rem;overflow-x:auto;white-space:pre-wrap;margin:.3rem 0}
pre.mermaid{background:var(--card);text-align:center}
.card{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:.6rem .8rem;
margin:.5rem 0}
.s-failed,.bad{color:var(--bad)}.s-succeeded{color:var(--ok)}.s-running{color:var(--run)}
.s-stale{color:var(--stale)}
.problem{background:var(--badbg);border-color:var(--bad)}
details summary{cursor:pointer}
"""

LIVE = """
let last = document.querySelector("main").innerHTML;
setInterval(async () => {
  try {
    const r = await fetch(location.href, {cache: "no-store"});
    if (!r.ok) return;
    const next = new DOMParser().parseFromString(await r.text(), "text/html")
      .querySelector("main");
    if (!next || next.innerHTML === last) return;
    last = next.innerHTML;
    const open = new Set([...document.querySelectorAll("details[open]")].map(d => d.id));
    next.querySelectorAll("details").forEach(d => { if (open.has(d.id)) d.open = true; });
    document.querySelector("main").replaceWith(next);
    if (window.mermaid) await window.mermaid.run({querySelector: "pre.mermaid"});
  } catch (err) {}
}, REFRESH);
"""


def layout(title: str, body: str, live: int | None = None, diagram: bool = False,
           nav: bool = True) -> str:
    script = ""
    if diagram:
        script += (f'<script type="module">import mermaid from "{MERMAID_JS}";'
                   "window.mermaid = mermaid;mermaid.initialize({startOnLoad: true, theme: "
                   "matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'default'});"
                   "</script>")
    if live:
        script += f"<script>{LIVE.replace('REFRESH', str(int(live * 1000)))}</script>"
    top = ('<nav><b>sluice</b><a href="/">Projects</a> · <a href="/fns">Functions</a></nav>'
           if nav else "")
    return (f'<!doctype html>\n<html><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width,initial-scale=1">'
            f"<title>sluice: {e(title)}</title><style>{CSS}</style></head>\n"
            f"<body>{top}<main>\n{body}\n</main>{script}</body></html>\n")


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


# ---- pages ------------------------------------------------------------------------------


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


def index(store: Store) -> str:
    rows = []
    for p in store.projects():
        rows.append(f'<tr><td><a href="/projects/{e(p["name"])}">{e(p["name"])}</a></td>'
                    f'<td>{e(p["description"])}</td><td>{_counts(p["counts"])}</td>'
                    f'<td>rev {p["rev"]}</td><td class="muted">{e(last_change(store, p["name"]))}'
                    f"</td></tr>")
    table = ('<div class="scroll"><table><tr><th>project</th><th>description</th><th>steps</th>'
             '<th>plan</th><th>last change</th></tr>' + "".join(rows) + "</table></div>"
             if rows else '<p class="muted">No projects yet.</p>')
    return layout("projects", f"<h1>Projects</h1>{table}", live=3)


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


def project_page(store: Store, project: str, doc: dict[str, Any], plan: Plan,
                 state: dict[str, Any], live: int | None = None) -> str:
    info = store.project(project)
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
            f'<tr><td><details id="step-{e(sid)}"><summary>{e(sid)}</summary>'
            f'{"".join(parts)}</details></td><td>{e(step.fn.name)}</td>'
            f'<td class="s-{e(entry["status"])}">{e(status)}</td>'
            f'<td>{e(entry.get("started") or "")}</td><td>{e(entry.get("finished") or "")}</td>'
            f'<td class="bad">{e(err[0]) if err else ""}</td></tr>')
    inputs = {n: state["inputs"].get(n) for n in plan.inputs}
    outputs = {n: value_of(r, plan, state)[1] for n, r in plan.outputs.items()}
    log_rows = []
    for x in reversed(store.history(project)[-HISTORY:]):
        what = x["kind"] + (f" ({len(x.get('ops') or [])} ops)" if "ops" in x else "")
        what += f" {x['step']}" if "step" in x else f" {x['name']}" if "name" in x else ""
        what += " (forced)" if x.get("force") else ""
        log_rows.append(f"<tr><td>{x['rev']}</td><td>{e(x['at'])}</td><td>{e(x['author'])}</td>"
                        f"<td>{e(what)}</td><td>{e(x.get('reason') or '')}</td></tr>")
    about = info.get("description") or ""
    steps_table = ('<div class="scroll"><table><tr><th>step</th><th>fn</th><th>status</th>'
                   "<th>started</th><th>finished</th><th>error</th></tr>" + "".join(rows)
                   + "</table></div>" if rows else '<p class="muted">The plan has no steps.</p>')
    body = f"""<h1>{e(project)} <small class="muted">rev {doc["rev"]}</small></h1>
{f'<p>{e(about)}</p>' if about else ''}
<pre class="mermaid">
{e(mermaid(plan, state))}</pre>
<h2>Inputs</h2>{_values_table(inputs)}
<h2>Outputs</h2>{_values_table(outputs)}
<h2>Steps</h2>{steps_table}
<h2>History</h2>
<div class="scroll"><table><tr><th>rev</th><th>at</th><th>author</th><th>what</th>
<th>reason</th></tr>{"".join(log_rows)}</table></div>"""
    return layout(project, body, live=live, diagram=True, nav=live is not None)


def render(store: Store, project: str, fmt: str, live: int | None = None) -> str:
    """plan_view and the project page: Mermaid text or the HTML page."""
    doc, plan = store.plan(project)
    state = store.read_state(project)
    if fmt == "mermaid":
        return mermaid(plan, state)
    if fmt == "html":
        return project_page(store, project, doc, plan, state, live)
    raise BadRequest(f'format must be "mermaid" or "html", got {fmt!r}')


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
