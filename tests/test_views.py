import re

from sluice import views
from tests.conftest import create


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


def test_the_html_page(store):
    create(store, "v", {"a": {"run": "test.add", "in": {"a": {"source": "n"}, "b": d(1)}},
                        "c": {"run": "test.boom", "in": {}}},
           inputs={"n": "int"}, outputs={"total": {"source": "a/sum"}})
    with store.lock("v"):
        store.write_state("v", {"inputs": {"n": 1}, "steps": {
            "a": {"status": "succeeded", "outputs": {"sum": 2}, "started": "T1", "finished": "T2"},
            "c": {"status": "failed", "error": "exit code 1\ntraceback <here>"}}})
    page = views.render(store, "v", "html", refresh=3)
    assert '<meta http-equiv="refresh" content="3">' in page
    assert "cdn.jsdelivr.net/npm/mermaid" in page
    assert '<pre class="mermaid">\nflowchart LR' in page
    assert ("<tr><td>a</td><td>test.add</td><td>succeeded</td><td>T1</td><td>T2</td><td></td></tr>"
            in page)
    assert "<td>c</td><td>test.boom</td><td>failed</td><td></td><td></td><td>exit code 1</td>" in page
    assert "traceback" not in page  # only the first line of the error
    assert "<tr><td>n</td><td><code>1</code></td></tr>" in page
    assert "<tr><td>total</td><td><code>2</code></td></tr>" in page
    assert "refresh" not in views.render(store, "v", "html")
