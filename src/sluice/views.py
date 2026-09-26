"""Views (SPEC §8): a plan and its state as a Mermaid flowchart, or as a standalone HTML page."""

from __future__ import annotations

import html
import json
from typing import Any

from .errors import BadRequest
from .plan import Plan, value_of
from .store import Store

CLASSES = {"pending": "fill:#f1f1f1,stroke:#999,color:#333",
           "running": "fill:#dbeafe,stroke:#2563eb,color:#1e3a8a",
           "succeeded": "fill:#dcfce7,stroke:#16a34a,color:#14532d",
           "failed": "fill:#fee2e2,stroke:#dc2626,color:#7f1d1d",
           "manual": "fill:#fff,stroke:#16a34a,stroke-width:3px,stroke-dasharray:6 3"}
MERMAID_JS = "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs"


def _q(text: str) -> str:
    return '"' + text.replace('"', "#quot;") + '"'


def step_label(sid: str, run: str, e: dict[str, Any]) -> str:
    status = e["status"]
    if "total" in e:
        status += f" {e.get('done', 0)}/{e['total']}"
    return f"{sid} / {run} / {status}"


def mermaid(plan: Plan, state: dict[str, Any]) -> str:
    ids = {("in", n): f"in{i}" for i, n in enumerate(plan.inputs)}
    ids.update({("step", s): f"s{i}" for i, s in enumerate(plan.steps)})
    ids.update({("out", n): f"out{i}" for i, n in enumerate(plan.outputs)})
    lines = ["flowchart LR"]
    for n in plan.inputs:
        lines.append(f"  {ids['in', n]}([{_q(n)}])")
    for sid, step in plan.steps.items():
        e = state["steps"].get(sid, {"status": "pending"})
        lines.append(f"  {ids['step', sid]}[{_q(step_label(sid, step.fn.name, e))}]")
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
        e = state["steps"].get(sid, {"status": "pending"})
        lines.append(f"  class {ids['step', sid]} {'manual' if e.get('manual') else e['status']}")
    return "\n".join(lines) + "\n"


def _values_table(values: dict[str, Any]) -> str:
    rows = "".join(f"<tr><td>{html.escape(k)}</td><td><code>{html.escape(json.dumps(v))}"
                   f"</code></td></tr>" for k, v in values.items())
    return f"<table><tr><th>name</th><th>value</th></tr>{rows}</table>" if rows else "<p>none</p>"


def page(pid: str, doc: dict[str, Any], plan: Plan, state: dict[str, Any],
         refresh: int | None = None) -> str:
    rows = []
    for sid, step in plan.steps.items():
        e = state["steps"].get(sid, {"status": "pending"})
        err = (e.get("error") or "").splitlines()[:1]
        status = step_label(sid, step.fn.name, e).rsplit(" / ", 1)[1]
        rows.append("<tr>" + "".join(f"<td>{html.escape(str(x))}</td>" for x in (
            sid, step.fn.name, status + (" (manual)" if e.get("manual") else ""),
            e.get("started") or "", e.get("finished") or "", err[0] if err else "")) + "</tr>")
    inputs = {n: state["inputs"].get(n) for n in plan.inputs}
    outputs = {n: value_of(r, plan, state)[1] for n, r in plan.outputs.items()}
    meta = f'<meta http-equiv="refresh" content="{refresh}">' if refresh else ""
    title = html.escape(doc.get("label") or pid)
    return f"""<!doctype html>
<html><head><meta charset="utf-8">{meta}<title>sluice: {title}</title>
<script type="module">import mermaid from "{MERMAID_JS}";
mermaid.initialize({{startOnLoad: true}});</script>
<style>body{{font-family:system-ui,sans-serif;margin:2rem;color:#222}}
table{{border-collapse:collapse}}td,th{{border:1px solid #ddd;padding:.3rem .6rem;text-align:left}}
code{{font-size:.9em}}</style></head>
<body><h1>{title} <small>{html.escape(pid)}, rev {doc["rev"]}</small></h1>
<pre class="mermaid">
{html.escape(mermaid(plan, state))}</pre>
<h2>Steps</h2>
<table><tr><th>step</th><th>fn</th><th>status</th><th>started</th><th>finished</th>
<th>error</th></tr>
{"".join(rows)}
</table>
<h2>Inputs</h2>{_values_table(inputs)}
<h2>Outputs</h2>{_values_table(outputs)}
</body></html>
"""


def render(store: Store, pid: str, fmt: str, refresh: int | None = None) -> str:
    doc, plan = store.plan(pid)
    state = store.read_state(pid)
    if fmt == "mermaid":
        return mermaid(plan, state)
    if fmt == "html":
        return page(pid, doc, plan, state, refresh)
    raise BadRequest(f'format must be "mermaid" or "html", got {fmt!r}')


def index(store: Store) -> str:
    items = "".join(
        f'<li><a href="/plans/{html.escape(p["id"])}">{html.escape(p["label"] or p["id"])}</a> '
        f"rev {p['rev']} {html.escape(json.dumps(p['counts']))}</li>" for p in store.plans())
    return (f'<!doctype html><html><head><meta charset="utf-8"><title>sluice plans</title>'
            f"</head><body><h1>Plans</h1><ul>{items or '<li>none</li>'}</ul></body></html>\n")
