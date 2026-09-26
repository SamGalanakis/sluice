import re

from sluice import views
from tests.conftest import create, write_fn


def d(x):
    return {"default": x}


def test_mermaid_shows_inputs_steps_outputs_edges_and_status_classes(store):
    create(store, "v", {
        "a": {"run": "test.add", "in": {"a": {"source": "n"}, "b": d(1)}},
        "b": {"run": "test.add", "in": {"a": {"source": "a/sum"}, "b": d(2)}},
        "c": {"run": "test.boom", "in": {}},
        "each": {"run": "test.window", "scatter": "tag",
                 "in": {"seconds": d(0), "tag": {"source": ["a/sum"]}}},
        "gate": {"run": "core.collect", "in": {"items": {"source": ["b/sum", "each/end"]}}},
    }, inputs={"n": "int"}, outputs={"all": {"source": "gate/items"}})
    with store.lock("v"):
        store.write_state("v", {"inputs": {"n": 1}, "steps": {
            "a": {"status": "succeeded", "outputs": {"sum": 2}},
            "b": {"status": "succeeded", "outputs": {"sum": 4}, "manual": True},
            "c": {"status": "failed", "error": "exit code 1"},
            "each": {"status": "running", "done": 1, "total": 3},
        }})
    text = views.render(store, "v", "mermaid")
    lines = text.splitlines()
    assert lines[0] == "flowchart LR"
    node = {m[1]: m[0] for m in re.findall(r'^\s+(\w+)[\[(]+"([^"]+)"', text, re.MULTILINE)}
    assert set(node) == {"n", "a / test.add / succeeded", "b / test.add / succeeded",
                         "c / test.boom / failed", "each / test.window / running 1/3",
                         "gate / core.collect / pending", "all"}
    assert f'  {node["n"]}(["n"])' in lines and f'  {node["all"]}(["all"])' in lines
    a, b, each, gate = (node[k] for k in ("a / test.add / succeeded", "b / test.add / succeeded",
                                          "each / test.window / running 1/3",
                                          "gate / core.collect / pending"))
    edges = {ln.strip() for ln in lines if "-->" in ln}
    assert edges == {f'{node["n"]} -->|"n"| {a}', f'{a} -->|"sum"| {b}', f'{a} -->|"sum"| {each}',
                     f'{b} -->|"sum"| {gate}', f'{each} -->|"end"| {gate}',
                     f'{gate} -->|"items"| {node["all"]}'}
    classes = dict(re.findall(r"^\s+class (\w+) (\w+)$", text, re.MULTILINE))
    assert classes == {a: "succeeded", b: "manual", node["c / test.boom / failed"]: "failed",
                       each: "running", gate: "pending"}
    for cls in ("pending", "running", "succeeded", "failed", "manual"):
        assert f"  classDef {cls} " in text


def test_the_project_page(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": {"source": "n"}, "b": d(1)}},
                        "c": {"run": "test.boom", "in": {}}},
           inputs={"n": "int"}, outputs={"total": {"source": "a/sum"}})
    run_dir = store.runs_dir("v") / "r1"
    run_dir.mkdir(parents=True)
    (run_dir / "input.json").write_text('{"a": 1, "b": 1}')
    (run_dir / "stderr.log").write_text("adding 1 + 1\n")
    with store.lock("v"):
        store.write_state("v", {"inputs": {"n": 1}, "steps": {
            "a": {"status": "succeeded", "outputs": {"sum": 2}, "started": "T1", "finished": "T2",
                  "run_ids": ["r1"]},
            "c": {"status": "failed", "error": "exit code 1\ntraceback <here>"}}})
    store.set_input("v", "n", 1, "me", "why not")
    page = views.project_page(store, "v", ver="abc")
    assert '<nav>' in page and views.DATASTAR_JS in page
    assert '<body data-signals="{&quot;ver&quot;: &quot;abc&quot;}">' in page
    assert "data-init=\"@get('/projects/v/stream', {retry: 'always'" in page
    assert "cdn.jsdelivr.net/npm/mermaid" in page
    assert '<pre class="mermaid">\nflowchart LR' in page
    assert '<pre id="plan-src" hidden data-view="plan-diagram" ' in page
    assert '<a href="/projects/v/log">' in page and '<div id="recent">' in page
    assert "the v project" in page
    assert ('<td class="s-succeeded">succeeded</td><td>T1</td><td>T2</td><td class="bad"></td>'
            in page)
    assert '<td class="s-failed">failed</td><td></td><td></td><td class="bad">exit code 1</td>' \
        in page
    step_a = page[page.index('<details id="step-a" data-preserve-attr="open">'):
                  page.index("</details>")]
    assert "bindings" in step_a and "&quot;source&quot;: &quot;n&quot;" in step_a
    assert "inputs</div><pre>{\n  &quot;a&quot;: 1," in step_a
    assert "outputs</div><pre>{\n  &quot;sum&quot;: 2\n}" in step_a
    assert "stderr (tail)</div><pre>adding 1 + 1</pre>" in step_a
    assert "traceback &lt;here&gt;" in page  # the full error, escaped, inside the details
    assert "<tr><td>n</td><td><code>1</code></td></tr>" in page
    assert "<tr><td>total</td><td><code>2</code></td></tr>" in page
    history = page[page.index("<h2>History</h2>"):]
    assert "<td>plan.input n</td><td>why not</td>" in history
    assert "<td>2</td>" in history and "<td>plan.edit (1 ops)</td>" in history  # newest first
    assert history.index("plan.input") < history.index("plan.edit")
    standalone = views.render(store, "v", "html")
    assert "<nav>" not in standalone and "datastar" not in standalone
    assert 'id="plan-src"' not in standalone and "/log" not in standalone
    assert '<pre class="mermaid">\nflowchart LR' in standalone



def test_an_empty_plan_shows_a_placeholder_not_mermaid_source(store):
    create(store, "v", {})
    page = views.project_page(store, "v", ver="abc")
    diagram = page[page.index('<div id="plan-diagram"'):]
    assert diagram.startswith('<div id="plan-diagram" class="diagram"><p class="muted">No steps')
    assert '<pre class="mermaid">' not in page
    assert '<pre id="plan-src" hidden data-empty data-view="plan-diagram" ' in page
    assert "No steps yet." in views.DIAGRAM_JS and "data-empty" in views.DIAGRAM_JS
    assert ".diagram pre.mermaid:not([data-processed]){visibility:hidden}" in views.CSS


def test_values_are_escaped(store):
    create(store, "v", {"a": {"run": "core.echo", "in": {"value": d("<script>x</script>")}}},
           outputs={"out": {"source": "a/value"}})
    store.update_project("v", "<b>bold</b>")
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {
            "a": {"status": "failed", "error": "<script>alert(1)</script>",
                  "outputs": {"value": "<script>alert(2)</script>"}}}})
    store.append("v", {"kind": "message", "thread": "t", "from": "<i>me</i>",
                        "body": "<script>alert(3)</script>"})
    for page in (views.project_page(store, "v", ver="x"), views.index(store, ver="x"),
                 views.log_page(store, "v", views.LogQuery())):
        assert "<script>alert" not in page and "<b>bold" not in page
        assert "<script>x" not in page
    page = views.render(store, "v", "html")
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in page
    assert "&lt;script&gt;alert(2)&lt;/script&gt;" in page
    assert "&lt;b&gt;bold&lt;/b&gt;" in page
    log = views.log_page(store, "v", views.LogQuery())
    assert "t from &lt;i&gt;me&lt;/i&gt;: &lt;script&gt;alert(3)&lt;/script&gt;" in log
    assert "&quot;body&quot;: &quot;&lt;script&gt;alert(3)&lt;/script&gt;&quot;" in log


def test_the_project_index(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}},
                        "b": {"run": "test.boom", "in": {}}})
    store.create_project("w", "second")
    with store.lock("v"):
        store.write_state("v", {"inputs": {}, "steps": {"a": {"status": "succeeded"},
                                                        "b": {"status": "failed"}}})
    page = views.index(store)
    assert '<a href="/projects/v">v</a>' in page and '<a href="/projects/w">w</a>' in page
    assert "the v project" in page and "second" in page
    assert '<span class="s-succeeded">1 succeeded</span>, <span class="s-failed">1 failed</span>' \
        in page
    assert re.search(r'<td class="muted">\d{4}-\d\d-\d\dT[\d:]+Z</td>', page)
    assert '<a href="/fns">Functions</a>' in page


def test_types_render_readably():
    assert views.type_text("string[]") == "string[]"
    assert views.type_text(["null", {"type": "enum", "symbols": ["a", "b"]}]) == "enum(a|b)?"
    assert views.type_text({"type": "array", "items": {"type": "record", "fields": {
        "f": "int", "g": "string?"}}}) == "{f: int, g: string?}[]"


def test_the_functions_page_groups_by_scope_and_shows_collisions(store):
    create(store, "v", {})
    write_fn(store.home / "fns", "mine.fn", {"xs": "string[]"},
             {"pick": {"type": "enum", "symbols": ["a", "b"]}}, spec={"doc": "<i>mine</i>"})
    write_fn(store.project_dir("v") / "fns", "test.add", {"a": "int"}, {"sum": "int"})
    write_fn(store.project_dir("v") / "fns", "v.local")
    page = views.fns_page(store, "v")
    sections = {m[0]: m[1] for m in re.findall(r"<h2>([^<]+)</h2>(.*?)(?=<h2>|</main>)", page,
                                               re.DOTALL)}
    assert set(sections) == {"Built-in", "Global", "Project (v)"}
    assert "<b>core.echo</b>" in sections["Built-in"] \
        and "<b>thread.post</b>" in sections["Built-in"]
    assert "<b>mine.fn</b>" in sections["Global"] and "<b>test.add</b>" in sections["Global"]
    assert "xs: <code>string[]</code>" in sections["Global"]
    assert "pick: <code>enum(a|b)</code>" in sections["Global"]
    assert "&lt;i&gt;mine&lt;/i&gt;" in sections["Global"]
    project = sections["Project (v)"]
    assert "<b>v.local</b>" in project
    clash = project[project.index('<div class="card problem">'):]
    assert "<b>test.add</b>" in clash and "fn test.add collides with the global fn" in clash
    plain = views.fns_page(store)
    assert "Project (" not in plain and "v.local" not in plain
